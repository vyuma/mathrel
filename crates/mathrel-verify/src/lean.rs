//! Lean 4 バックエンド。
//!
//! 3 つに分けてある。
//!
//! 1. [`LeanVerifier::render_source`] — 義務から Lean のソースを組み立てる（純粋）
//! 2. 外部プロセスの起動 — [`crate::runner::CommandRunner`] 越し
//! 3. [`LeanVerifier::interpret`] — 出力から判定を作る（純粋）
//!
//! 1 と 3 が純粋なので、**Lean が入っていなくても振る舞いを固定できる**。
//!
//! ## 呼び出し方
//!
//! Lean 4 の `lean` はファイルを引数に取る。**標準入力からソースを読ませる
//! 呼び出し方はない。** したがって一時ファイルへ書き出し、そのパスを引数に
//! 渡す。既定は Mathlib を使う前提で `lake env lean <path>` である。
//!
//! | 用途 | コマンド |
//! |---|---|
//! | Mathlib あり（既定） | `lake env lean <path>` |
//! | import をビルドしてから走らせる | `lake lean <path>` |
//! | Mathlib なし | `lean <path>` |
//!
//! [`LeanVerifier::with_command`] で差し替えられる。
//!
//! ## `sorry` の検出
//!
//! 警告文「declaration uses 'sorry'」の一致に頼ってはならない。文言も引用符も
//! Lean の版で変わり得るし、ラッパを噛ませると消えることもある。壊れたときに
//! **`sorry` 入りの証明が `Checked` になる**ので、失敗の向きが最悪である。
//!
//! 代わりに、生成したソースの末尾へ `#print axioms <名前>` を置き、その出力に
//! `sorryAx` が現れるかを見る。これは Lean のカーネルが実際に記録している
//! 公理依存であり、文言ではない。警告文の一致も残してあるが、それは保険で
//! あって主たる根拠ではない。
//!
//! さらに [`Trust::Checked`] は**満たすべき条件を全部並べた上での結論**に
//! してある（[`LeanVerifier::interpret`]）。判定が壊れる方向を、常に
//! 「信頼度が下がる側」に倒すためである。

use crate::obligation::{BackendId, Obligation};
use crate::runner::{CommandOutput, CommandRunner, RunError};
use crate::trust::Trust;
use crate::verdict::Verdict;
use crate::verifier::Verifier;
use std::sync::atomic::{AtomicU64, Ordering};

/// Lean が `sorry` を含む宣言に対して出す警告。保険として見るだけ。
const SORRY_WARNING: &str = "uses 'sorry'";
/// `#print axioms` の出力に現れる、`sorry` の正体。
const SORRY_AXIOM: &str = "sorryAx";

/// 一時ファイル名の重複を避けるための連番。
static NONCE: AtomicU64 = AtomicU64::new(0);

/// Lean 4 を呼ぶ検証器。
#[derive(Clone, Debug)]
pub struct LeanVerifier<R: CommandRunner> {
    runner: R,
    program: String,
    args: Vec<String>,
    /// 版。**Mathlib のリビジョンもここに含める**。含めないと、Mathlib を
    /// 更新しても指紋が変わらず、古い判定が `Clean` のまま残る。
    version: String,
    /// ソースの先頭に置く行（`import Mathlib` など）。
    preamble: String,
    /// 一時ファイルを置く場所。
    workdir: std::path::PathBuf,
}

impl<R: CommandRunner> LeanVerifier<R> {
    /// 検証器を作る。
    ///
    /// `version` は省略できない。**省略可能にすると、既定値のまま使われて
    /// Lean や Mathlib を更新しても指紋が変わらず、古い判定が `Clean` の
    /// まま残る。** 型で忘れられないようにしてある。
    ///
    /// 判定を左右するものはすべて入れること。最低でも Lean の版と、Mathlib
    /// を使うならそのリビジョン。
    ///
    /// ```
    /// # use mathrel_verify::{LeanVerifier, ScriptedRunner};
    /// let verifier = LeanVerifier::new(ScriptedRunner::default(), "4.8.0+mathlib@deadbeef");
    /// ```
    pub fn new(runner: R, version: &str) -> Self {
        Self {
            runner,
            program: "lake".to_owned(),
            args: vec!["env".to_owned(), "lean".to_owned()],
            version: version.to_owned(),
            preamble: String::new(),
            workdir: std::env::temp_dir(),
        }
    }

    /// 起動するコマンドを差し替える。ソースのパスは末尾に足される。
    #[must_use]
    pub fn with_command(mut self, program: &str, args: &[&str]) -> Self {
        self.program = program.to_owned();
        self.args = args.iter().map(|arg| (*arg).to_owned()).collect();
        self
    }

    /// ソースの先頭に置く行を設定する。
    #[must_use]
    pub fn with_preamble(mut self, preamble: &str) -> Self {
        self.preamble = preamble.to_owned();
        self
    }

    /// 一時ファイルを置く場所を変える。Lake プロジェクトの中に置きたいとき。
    #[must_use]
    pub fn with_workdir(mut self, workdir: impl Into<std::path::PathBuf>) -> Self {
        self.workdir = workdir.into();
        self
    }

    /// 義務から Lean のソースを組み立てる。
    ///
    /// `context` は上流の定義・補題の本文で、依存順に並んでいる。カーネルが
    /// 依存順を決めているので、ここで並べ替える必要はない。
    ///
    /// 証明がない義務は `sorry` を置く。Lean は通すが、末尾の
    /// `#print axioms` が `sorryAx` を報告するので `Checked` にはならない。
    #[must_use]
    pub fn render_source(&self, obligation: &Obligation, context: &[String]) -> String {
        let mut source = String::new();
        if !self.preamble.is_empty() {
            source.push_str(&self.preamble);
            source.push('\n');
        }
        for entry in context {
            source.push_str(entry);
            source.push('\n');
        }
        source.push_str("theorem ");
        source.push_str(&obligation.name);
        source.push_str(" : ");
        source.push_str(&obligation.statement);
        source.push_str(" := ");
        source.push_str(obligation.proof.as_deref().unwrap_or("sorry"));
        source.push('\n');
        // カーネルが記録している公理依存を吐かせる。`sorry` の検出はこれが
        // 主たる根拠であり、警告文の一致は保険にすぎない。
        source.push_str("#print axioms ");
        source.push_str(&obligation.name);
        source.push('\n');
        source
    }

    /// 実行結果から判定を作る。
    ///
    /// `proof_supplied` は、義務が自前の証明を持っていたか。持っていなければ
    /// `sorry` を置いたのはこちらなので、Lean の出力を見るまでもなく
    /// [`Trust::Checked`] ではあり得ない。
    ///
    /// [`Trust::Checked`] は**条件を全部並べた上での結論**にしてある。判定が
    /// 壊れたときに信頼度が上がる側へ倒れないようにするためである。
    #[must_use]
    pub fn interpret(
        result: &Result<CommandOutput, RunError>,
        proof_supplied: bool,
    ) -> Verdict {
        let output = match result {
            Ok(output) => output,
            Err(RunError::NotFound(program)) => {
                return Verdict::Unavailable {
                    reason: format!("{program} が見つかりません。Lean 4 を入れてください"),
                }
            }
            Err(RunError::Timeout(limit)) => {
                return Verdict::Unknown {
                    reason: format!("{} 秒で打ち切りました", limit.as_secs()),
                }
            }
            Err(RunError::Io(message)) => {
                return Verdict::Unavailable {
                    reason: message.clone(),
                }
            }
        };

        let combined = output.combined();
        if !output.succeeded() || combined.contains("error:") {
            return Verdict::Refuted {
                reason: first_error(&combined),
            };
        }

        let checked = proof_supplied
            && !uses_sorry_axiom(&combined)
            && !combined.contains(SORRY_WARNING);

        Verdict::Proved {
            trust: if checked {
                Trust::Checked
            } else {
                Trust::Assumed
            },
        }
    }
}

/// `#print axioms` の出力に `sorryAx` が現れるか。
///
/// 出力は `'thm' depends on axioms: [propext, sorryAx]` の形。公理が 1 つも
/// なければ `'thm' does not depend on any axioms` になる。
///
/// 引用した補題は `axiom` として宣言してあるので、ここに名前が出る。それは
/// 正常であり、信頼度は [`crate::ProofSpace::effective_trust`] が合成する。
/// ここで見るのは `sorryAx` だけである。
fn uses_sorry_axiom(combined: &str) -> bool {
    combined
        .lines()
        .filter(|line| line.contains("depends on axioms"))
        .any(|line| line.contains(SORRY_AXIOM))
}

/// 出力から、AI へ差し戻すのに使える部分を切り出す。
///
/// 全部返すと Mathlib のビルドログまで混ざる。`error:` を含む行から後ろを
/// 適度な長さで返す。
fn first_error(combined: &str) -> String {
    let lines: Vec<&str> = combined.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains("error:"))
        .unwrap_or(0);
    let excerpt: Vec<&str> = lines[start..].iter().take(20).copied().collect();
    let excerpt = excerpt.join("\n");
    if excerpt.trim().is_empty() {
        "Lean が異常終了しました（出力なし）".to_owned()
    } else {
        excerpt
    }
}

/// ソースを置く一時ファイルの名前。呼び出しごとに違うものにする。
fn temp_name(obligation: &Obligation) -> String {
    let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
    let safe: String = obligation
        .name
        .chars()
        .map(|ch| if ch.is_alphanumeric() { ch } else { '_' })
        .collect();
    format!("mathrel_{safe}_{}_{nonce}.lean", std::process::id())
}

impl<R: CommandRunner> Verifier for LeanVerifier<R> {
    fn backend(&self) -> BackendId {
        BackendId::new("lean4", &self.version)
    }

    fn verify(&self, obligation: &Obligation, context: &[String]) -> Verdict {
        let source = self.render_source(obligation, context);
        let path = self.workdir.join(temp_name(obligation));

        if let Err(error) = std::fs::write(&path, &source) {
            // 書けないなら「偽」ではない。道具立ての問題である。
            return Verdict::Unavailable {
                reason: format!("{} に書けませんでした: {error}", path.display()),
            };
        }

        let path_string = path.to_string_lossy().into_owned();
        let mut args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        args.push(&path_string);
        let result = self.runner.run(&self.program, &args, "");

        let _ = std::fs::remove_file(&path);
        Self::interpret(&result, obligation.proof.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::ScriptedRunner;
    use std::time::Duration;

    fn obligation() -> Obligation {
        Obligation::new("f_nonneg", "∀ t : ℝ, 0 ≤ f t").with_proof("by positivity")
    }

    fn ok(stdout: &str) -> Result<CommandOutput, RunError> {
        Ok(CommandOutput {
            status: Some(0),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }

    /// Lean が「公理に依存していない」と答えたときの出力。
    fn clean_axioms(name: &str) -> String {
        format!("'{name}' depends on axioms: [propext, Classical.choice, Quot.sound]")
    }

    // --- ソースの組み立て ---

    #[test]
    fn the_source_contains_preamble_context_and_theorem_in_order() {
        let verifier = LeanVerifier::new(ScriptedRunner::default(), "4.8.0")
            .with_preamble("import Mathlib");
        let source = verifier.render_source(&obligation(), &["def f (t : ℝ) : ℝ := t^2".to_owned()]);

        let preamble = source.find("import").expect("preamble");
        let context = source.find("def f").expect("context");
        let theorem = source.find("theorem f_nonneg").expect("theorem");
        assert!(preamble < context && context < theorem, "{source}");
        assert!(source.contains(":= by positivity"), "{source}");
    }

    /// 公理依存を吐かせる行が必ず末尾に付く。`sorry` 検出の根拠になる。
    #[test]
    fn the_source_asks_lean_for_the_axiom_dependencies() {
        let verifier = LeanVerifier::new(ScriptedRunner::default(), "4.8.0");
        let source = verifier.render_source(&obligation(), &[]);
        assert!(source.trim_end().ends_with("#print axioms f_nonneg"), "{source}");
    }

    #[test]
    fn a_missing_proof_becomes_sorry() {
        let verifier = LeanVerifier::new(ScriptedRunner::default(), "4.8.0");
        let source = verifier.render_source(&Obligation::new("thm", "True"), &[]);
        assert!(source.contains(":= sorry"), "{source}");
    }

    // --- 呼び出し方 ---

    /// Lean 4 に標準入力からソースを読ませる方法はない。ファイルへ書いて
    /// パスを渡す。既定は Mathlib を使う前提の `lake env lean`。
    #[test]
    fn the_source_goes_to_a_file_whose_path_is_passed_as_an_argument() {
        let runner = ScriptedRunner::new(vec![ok(&clean_axioms("f_nonneg"))]);
        let verifier = LeanVerifier::new(runner, "4.8.0");
        verifier.verify(&obligation(), &[]);

        let calls = verifier.runner.calls();
        assert_eq!(calls[0].0, "lake");
        assert_eq!(calls[0].1[..2], ["env".to_owned(), "lean".to_owned()]);
        assert!(
            calls[0].1.last().expect("パス").ends_with(".lean"),
            "{:?}", calls[0].1
        );
        assert_eq!(calls[0].2, "", "標準入力は使わない");

        let seen = verifier.runner.files_seen();
        assert_eq!(seen.len(), 1, "Lean が読むファイルが実在する");
        assert!(seen[0].contains("theorem f_nonneg"), "{}", seen[0]);
    }

    #[test]
    fn the_temporary_file_is_removed_afterwards() {
        let runner = ScriptedRunner::new(vec![ok(&clean_axioms("f_nonneg"))]);
        let verifier = LeanVerifier::new(runner, "4.8.0");
        verifier.verify(&obligation(), &[]);

        let path = verifier.runner.calls()[0].1.last().expect("パス").clone();
        assert!(!std::path::Path::new(&path).exists(), "後片付けする");
    }

    #[test]
    fn the_command_can_be_replaced() {
        let runner = ScriptedRunner::new(vec![ok(&clean_axioms("f_nonneg"))]);
        let verifier = LeanVerifier::new(runner, "4.8.0").with_command("lean", &[]);
        verifier.verify(&obligation(), &[]);

        let calls = verifier.runner.calls();
        assert_eq!(calls[0].0, "lean");
        assert_eq!(calls[0].1.len(), 1, "パスだけが渡る");
    }

    // --- 出力の解釈 ---

    #[test]
    fn a_clean_run_with_no_sorry_is_checked() {
        assert_eq!(
            LeanVerifier::<ScriptedRunner>::interpret(&ok(&clean_axioms("thm")), true),
            Verdict::Proved {
                trust: Trust::Checked
            }
        );
    }

    /// `#print axioms` が `sorryAx` を報告したら `Checked` にしない。
    /// 警告文が出ていなくても、である。
    #[test]
    fn a_sorry_axiom_blocks_checked_even_without_the_warning() {
        let output = ok("'thm' depends on axioms: [propext, sorryAx]");
        let verdict = LeanVerifier::<ScriptedRunner>::interpret(&output, true);
        assert!(verdict.is_proved());
        assert_eq!(verdict.trust(), Trust::Assumed);
    }

    /// 警告文だけが出ている場合も拾う（保険）。
    #[test]
    fn the_warning_text_is_still_honoured_as_a_backstop() {
        let output = ok("stdin:2:0: warning: declaration uses 'sorry'");
        assert_eq!(
            LeanVerifier::<ScriptedRunner>::interpret(&output, true).trust(),
            Trust::Assumed
        );
    }

    /// 証明を持たない義務は、Lean が何と言おうと `Checked` にならない。
    /// `sorry` を置いたのはこちらだからである。
    #[test]
    fn an_obligation_we_filled_with_sorry_is_never_checked() {
        // Lean が完全に沈黙した（警告も公理報告も出なかった）場合。
        let verdict = LeanVerifier::<ScriptedRunner>::interpret(&ok(""), false);
        assert!(verdict.is_proved());
        assert_eq!(
            verdict.trust(),
            Trust::Assumed,
            "出力の解釈に頼らずに決まる"
        );
    }

    /// 引用した補題は `axiom` なので公理一覧に出る。それは正常である。
    #[test]
    fn cited_lemmas_appearing_as_axioms_do_not_downgrade_trust() {
        let output = ok("'main' depends on axioms: [propext, f_refl, Quot.sound]");
        assert_eq!(
            LeanVerifier::<ScriptedRunner>::interpret(&output, true).trust(),
            Trust::Checked,
            "信頼度の合成は effective_trust の仕事であって、ここではない"
        );
    }

    #[test]
    fn an_error_is_refuted_with_the_message_kept() {
        let output = Ok(CommandOutput {
            status: Some(1),
            stdout: "Check.lean:3:0: error: unknown identifier 'g'".to_owned(),
            stderr: String::new(),
        });
        let verdict = LeanVerifier::<ScriptedRunner>::interpret(&output, true);
        assert_eq!(verdict.kind_str(), "Refuted");
        assert!(
            verdict.reason().expect("理由").contains("unknown identifier"),
            "AI に差し戻せるだけの情報を残す"
        );
    }

    #[test]
    fn an_error_with_a_zero_exit_is_still_refuted() {
        let output = ok("Check.lean:1:0: error: type mismatch");
        assert_eq!(
            LeanVerifier::<ScriptedRunner>::interpret(&output, true).kind_str(),
            "Refuted"
        );
    }

    /// 道具がないことと、偽であることは別である。
    #[test]
    fn a_missing_lean_is_unavailable_not_refuted() {
        let result = Err(RunError::NotFound("lake".to_owned()));
        let verdict = LeanVerifier::<ScriptedRunner>::interpret(&result, true);
        assert!(verdict.is_unavailable());
        assert!(verdict.reason().expect("理由").contains("Lean 4"));
    }

    #[test]
    fn a_timeout_is_unknown_not_refuted() {
        let result = Err(RunError::Timeout(Duration::from_secs(60)));
        assert_eq!(
            LeanVerifier::<ScriptedRunner>::interpret(&result, true).kind_str(),
            "Unknown"
        );
    }

    /// 一時ファイルが書けなくても「偽」にはしない。
    #[test]
    fn an_unwritable_workdir_is_unavailable() {
        let runner = ScriptedRunner::new(vec![ok("")]);
        let verifier = LeanVerifier::new(runner, "4.8.0")
            .with_workdir("/proc/mathrel-does-not-exist");
        let verdict = verifier.verify(&obligation(), &[]);
        assert!(verdict.is_unavailable(), "{verdict:?}");
        assert!(verifier.runner.calls().is_empty(), "起動まで行かない");
    }

    #[test]
    fn the_error_excerpt_starts_at_the_first_error_line() {
        let noise = (0..50)
            .map(|index| format!("info: building {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let combined = format!("{noise}\nCheck.lean:1:0: error: 本題\nmore");
        let excerpt = first_error(&combined);
        assert!(excerpt.starts_with("Check.lean:1:0: error: 本題"), "{excerpt}");
        assert!(!excerpt.contains("building 0"));
    }

    #[test]
    fn an_empty_failure_still_says_something() {
        let output = Ok(CommandOutput {
            status: Some(2),
            stdout: String::new(),
            stderr: String::new(),
        });
        let verdict = LeanVerifier::<ScriptedRunner>::interpret(&output, true);
        assert_eq!(verdict.kind_str(), "Refuted");
        assert!(!verdict.reason().expect("理由").is_empty());
    }

    #[test]
    fn the_backend_id_carries_the_declared_version() {
        let verifier =
            LeanVerifier::new(ScriptedRunner::default(), "4.8.0+mathlib@deadbeef");
        assert_eq!(verifier.backend().id, "lean4");
        assert_eq!(verifier.backend().version, "4.8.0+mathlib@deadbeef");
    }

    /// 一時ファイル名は呼び出しごとに違う。並行して走らせても衝突しない。
    #[test]
    fn temporary_names_do_not_collide() {
        let first = temp_name(&obligation());
        let second = temp_name(&obligation());
        assert_ne!(first, second);
        assert!(first.ends_with(".lean"));
    }
}
