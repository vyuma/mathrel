//! AI の口と、証明修復ループ。
//!
//! ## AI は提案し、検証器が判定する
//!
//! [`ProofSynthesizer`] は文字列しか返せない。[`crate::verdict::Verdict`] を
//! 返す手段を型として持たない。これは意図的な制約である
//! （`docs/検証層の設計.md` ADR-007）。
//!
//! 型で分けておかないと、いつか「AI が高確度で正しいと言ったから通す」経路が
//! 生える。そうなった時点で、この道具が出す「証明済み」は意味を失う。

use crate::obligation::Obligation;
use crate::runner::CommandRunner;
use crate::verdict::Verdict;
use crate::verifier::Verifier;

/// 1 回の試行。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attempt {
    /// AI が出した証明。
    pub proof: String,
    /// 検証器の判定。
    pub verdict: Verdict,
}

impl Attempt {
    /// 検証器が返した理由。次の提案への差し戻しに使う。
    #[must_use]
    pub fn feedback(&self) -> &str {
        self.verdict.reason().unwrap_or("")
    }
}

/// 証明の候補を出すもの。**判定はしない。**
pub trait ProofSynthesizer {
    /// 実装の名前。記録用。
    fn id(&self) -> &str;

    /// 証明の候補を 1 つ返す。出せなければ `None`。
    ///
    /// `feedback` には、これまでの失敗が検証器の出力つきで入っている。
    /// これが探索ループの燃料になる。
    fn propose(
        &self,
        obligation: &Obligation,
        context: &[String],
        feedback: &[Attempt],
    ) -> Option<String>;
}

/// 修復の結果。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RepairOutcome {
    /// 最終的な義務。成功していれば通った証明が入っている。
    pub obligation: Obligation,
    /// 最終的な判定。**検証器が出したもの**であり、AI の自己申告ではない。
    pub verdict: Verdict,
    /// 試行の記録。
    pub attempts: Vec<Attempt>,
}

impl RepairOutcome {
    /// 修復できたか。
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.verdict.is_proved()
    }
}

/// 壊れた証明を、AI に出させて検証器にかける、を繰り返す。
///
/// 打ち切り条件は 3 つ。
///
/// 1. 検証器が通した
/// 2. `max_attempts` 回試した
/// 3. **検証器が使えなかった**（`Unavailable`）
///
/// 3 が要る。Lean が入っていない環境で AI を 5 回呼んでも、判定できないことに
/// 変わりはない。時間と金を捨てるだけである。
pub fn repair(
    obligation: &Obligation,
    context: &[String],
    verifier: &dyn Verifier,
    synthesizer: &dyn ProofSynthesizer,
    max_attempts: usize,
) -> RepairOutcome {
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut current = obligation.clone();

    for _ in 0..max_attempts {
        let proof = match synthesizer.propose(obligation, context, &attempts) {
            Some(proof) => proof,
            None => break,
        };
        current = obligation.clone().with_proof(&proof);
        let verdict = verifier.verify(&current, context);

        if verdict.is_unavailable() {
            attempts.push(Attempt {
                proof,
                verdict: verdict.clone(),
            });
            return RepairOutcome {
                obligation: current,
                verdict,
                attempts,
            };
        }

        let proved = verdict.is_proved();
        attempts.push(Attempt {
            proof,
            verdict: verdict.clone(),
        });
        if proved {
            return RepairOutcome {
                obligation: current,
                verdict,
                attempts,
            };
        }
    }

    let verdict = attempts
        .last()
        .map(|attempt| attempt.verdict.clone())
        .unwrap_or(Verdict::Unknown {
            reason: "証明の候補が出ませんでした".to_owned(),
        });
    RepairOutcome {
        obligation: current,
        verdict,
        attempts,
    }
}

// ---------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------

/// `codex exec` を呼んで証明の候補を出させる実装。
///
/// ## MCP について
///
/// `codex mcp add lean -- <lean-mcp-server>` のように MCP サーバを登録すると、
/// Codex 自身が Lean を叩けるようになり、提案の質は上がる。**しかし信頼度は
/// 上がらない。** Codex が自分で確かめたと言っても、それは自己申告である。
/// `Trust::Checked` になるのは、こちらの [`Verifier`] が通したときだけである。
///
/// MCP は探索の効率の話であり、信頼の話ではない。
#[derive(Debug)]
pub struct CodexSynthesizer<R: CommandRunner> {
    runner: R,
    program: String,
    args: Vec<String>,
}

impl<R: CommandRunner> CodexSynthesizer<R> {
    /// 既定の設定で作る。
    ///
    /// 読み取り専用サンドボックスで、セッションを残さずに 1 回実行する。
    /// 証明の候補を出すのに書き込み権限は要らない。
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            program: "codex".to_owned(),
            args: [
                "exec",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--ephemeral",
                "--color",
                "never",
                "-",
            ]
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect(),
        }
    }

    /// 起動するコマンドを差し替える。
    #[must_use]
    pub fn with_command(mut self, program: &str, args: &[&str]) -> Self {
        self.program = program.to_owned();
        self.args = args.iter().map(|arg| (*arg).to_owned()).collect();
        self
    }

    /// Codex へ渡す指示を組み立てる。
    ///
    /// 曖昧な依頼をすると散文が返ってくる。出力の形を先に固定する。
    #[must_use]
    pub fn render_prompt(
        obligation: &Obligation,
        context: &[String],
        feedback: &[Attempt],
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str(
            "あなたは Lean 4 の証明を書く。出力は ```lean で囲んだ証明項のみとし、\n\
             説明を書いてはならない。`sorry` を使ってはならない。\n\
             出力するのは `:=` の右側だけである（`theorem` 行は書かない）。\n\n",
        );

        if !context.is_empty() {
            prompt.push_str("## 文脈（既に定義されているもの）\n\n```lean\n");
            for entry in context {
                prompt.push_str(entry);
                prompt.push('\n');
            }
            prompt.push_str("```\n\n");
        }

        prompt.push_str("## 証明する命題\n\n```lean\ntheorem ");
        prompt.push_str(&obligation.name);
        prompt.push_str(" : ");
        prompt.push_str(&obligation.statement);
        prompt.push_str(" := ?\n```\n");

        if !feedback.is_empty() {
            prompt.push_str("\n## これまでの失敗\n\n");
            for (index, attempt) in feedback.iter().enumerate() {
                prompt.push_str(&format!(
                    "### 試行 {}\n\n提出した証明:\n\n```lean\n{}\n```\n\nLean の出力:\n\n```\n{}\n```\n\n",
                    index + 1,
                    attempt.proof,
                    attempt.feedback()
                ));
            }
            prompt.push_str("同じ誤りを繰り返さないこと。\n");
        }

        prompt
    }

    /// Codex の出力から証明を取り出す。
    ///
    /// 指示しても散文が混ざることがあるので、緩く拾う。
    #[must_use]
    pub fn extract_proof(output: &str) -> Option<String> {
        if let Some(block) = fenced_block(output) {
            return Some(block);
        }
        // 囲みがない場合、`by ...` で始まる行を拾う。
        output
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("by ") || *line == "by")
            .map(str::to_owned)
    }
}

/// 最初の ``` 囲みの中身を返す。言語名の行（```lean など）は落とす。
fn fenced_block(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let mut body: Vec<&str> = Vec::new();
    let mut inside = false;

    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if inside {
                let joined = body.join("\n").trim().to_owned();
                return if joined.is_empty() { None } else { Some(joined) };
            }
            inside = true;
            continue;
        }
        if inside {
            body.push(line);
        }
    }
    None
}

impl<R: CommandRunner> ProofSynthesizer for CodexSynthesizer<R> {
    fn id(&self) -> &str {
        "codex"
    }

    fn propose(
        &self,
        obligation: &Obligation,
        context: &[String],
        feedback: &[Attempt],
    ) -> Option<String> {
        let prompt = Self::render_prompt(obligation, context, feedback);
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        let output = self.runner.run(&self.program, &args, &prompt).ok()?;
        Self::extract_proof(&output.combined())
    }
}

/// テスト用。決められた候補を順に返す。
#[derive(Debug)]
pub struct ScriptedSynthesizer {
    proposals: std::cell::RefCell<std::collections::VecDeque<String>>,
    seen_feedback: std::cell::RefCell<Vec<usize>>,
}

impl ScriptedSynthesizer {
    /// 候補の並びを与えて作る。
    #[must_use]
    pub fn new(proposals: &[&str]) -> Self {
        Self {
            proposals: std::cell::RefCell::new(
                proposals.iter().map(|item| (*item).to_owned()).collect(),
            ),
            seen_feedback: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// 各回の呼び出しで見た失敗の件数。
    #[must_use]
    pub fn seen_feedback(&self) -> Vec<usize> {
        self.seen_feedback.borrow().clone()
    }
}

impl ProofSynthesizer for ScriptedSynthesizer {
    fn id(&self) -> &str {
        "scripted"
    }

    fn propose(
        &self,
        _obligation: &Obligation,
        _context: &[String],
        feedback: &[Attempt],
    ) -> Option<String> {
        self.seen_feedback.borrow_mut().push(feedback.len());
        self.proposals.borrow_mut().pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lean::LeanVerifier;
    use crate::obligation::BackendId;
    use crate::runner::{CommandOutput, RunError, ScriptedRunner};
    use crate::trust::Trust;

    /// 決められた判定を順に返す検証器。
    struct ScriptedVerifier {
        verdicts: std::cell::RefCell<std::collections::VecDeque<Verdict>>,
    }

    impl ScriptedVerifier {
        fn new(verdicts: Vec<Verdict>) -> Self {
            Self {
                verdicts: std::cell::RefCell::new(verdicts.into()),
            }
        }
    }

    impl Verifier for ScriptedVerifier {
        fn backend(&self) -> BackendId {
            BackendId::new("scripted", "1")
        }
        fn verify(&self, _obligation: &Obligation, _context: &[String]) -> Verdict {
            self.verdicts
                .borrow_mut()
                .pop_front()
                .unwrap_or(Verdict::Unknown {
                    reason: "判定が尽きました".to_owned(),
                })
        }
    }

    fn broken() -> Obligation {
        Obligation::new("thm", "2 + 2 = 4")
    }

    // --- 修復ループ ---

    #[test]
    fn a_failed_attempt_is_retried_with_the_error_fed_back() {
        let verifier = ScriptedVerifier::new(vec![
            Verdict::Refuted {
                reason: "unknown identifier 'foo'".to_owned(),
            },
            Verdict::Proved {
                trust: Trust::Checked,
            },
        ]);
        let synthesizer = ScriptedSynthesizer::new(&["by foo", "by norm_num"]);

        let outcome = repair(&broken(), &[], &verifier, &synthesizer, 5);

        assert!(outcome.succeeded());
        assert_eq!(outcome.attempts.len(), 2);
        assert_eq!(outcome.obligation.proof.as_deref(), Some("by norm_num"));
        assert_eq!(
            synthesizer.seen_feedback(),
            vec![0, 1],
            "2 回目の提案は 1 件の失敗を見ている"
        );
        assert!(outcome.attempts[0].feedback().contains("unknown identifier"));
    }

    #[test]
    fn the_loop_stops_at_max_attempts() {
        let verifier = ScriptedVerifier::new(vec![
            Verdict::Refuted {
                reason: "だめ".to_owned(),
            },
            Verdict::Refuted {
                reason: "だめ".to_owned(),
            },
            Verdict::Refuted {
                reason: "だめ".to_owned(),
            },
        ]);
        let synthesizer = ScriptedSynthesizer::new(&["a", "b", "c"]);

        let outcome = repair(&broken(), &[], &verifier, &synthesizer, 2);

        assert!(!outcome.succeeded());
        assert_eq!(outcome.attempts.len(), 2, "3 回目は呼ばない");
    }

    /// 検証器が使えないなら、AI を呼び続けても無意味である。
    #[test]
    fn an_unavailable_verifier_stops_the_loop_immediately() {
        let verifier = ScriptedVerifier::new(vec![
            Verdict::Unavailable {
                reason: "lean が見つかりません".to_owned(),
            },
            Verdict::Proved {
                trust: Trust::Checked,
            },
        ]);
        let synthesizer = ScriptedSynthesizer::new(&["a", "b", "c"]);

        let outcome = repair(&broken(), &[], &verifier, &synthesizer, 5);

        assert!(!outcome.succeeded());
        assert!(outcome.verdict.is_unavailable());
        assert_eq!(outcome.attempts.len(), 1, "1 回で止める");
    }

    #[test]
    fn a_synthesizer_with_nothing_to_say_ends_the_loop() {
        let verifier = ScriptedVerifier::new(vec![]);
        let synthesizer = ScriptedSynthesizer::new(&[]);

        let outcome = repair(&broken(), &[], &verifier, &synthesizer, 5);

        assert!(outcome.attempts.is_empty());
        assert_eq!(outcome.verdict.kind_str(), "Unknown");
    }

    /// AI が出した証明でも、信頼度は検証器が決める。
    #[test]
    fn the_ai_never_upgrades_its_own_trust() {
        let verifier = ScriptedVerifier::new(vec![Verdict::Proved {
            trust: Trust::Assumed,
        }]);
        let synthesizer = ScriptedSynthesizer::new(&["by sorry"]);

        let outcome = repair(&broken(), &[], &verifier, &synthesizer, 3);

        assert_eq!(
            outcome.verdict.trust(),
            Trust::Assumed,
            "検証器が Assumed と言えば Assumed のまま"
        );
    }

    /// 修復ループと Lean バックエンドが繋がることを、Lean なしで確かめる。
    #[test]
    fn the_loop_drives_the_lean_backend() {
        let runner = ScriptedRunner::new(vec![
            Ok(CommandOutput {
                status: Some(1),
                stdout: "stdin:1:0: error: unknown tactic 'foo'".to_owned(),
                stderr: String::new(),
            }),
            Ok(CommandOutput {
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            }),
        ]);
        let verifier = LeanVerifier::new(runner, "4.8.0");
        let synthesizer = ScriptedSynthesizer::new(&["by foo", "by norm_num"]);

        let outcome = repair(&broken(), &[], &verifier, &synthesizer, 4);

        assert!(outcome.succeeded());
        assert_eq!(outcome.verdict.trust(), Trust::Checked);
        assert!(outcome.attempts[0].feedback().contains("unknown tactic"));
    }

    // --- Codex ---

    #[test]
    fn the_prompt_pins_down_the_output_shape() {
        let prompt = CodexSynthesizer::<ScriptedRunner>::render_prompt(&broken(), &[], &[]);
        assert!(prompt.contains("```lean"));
        assert!(prompt.contains("sorry"), "sorry の禁止を書く");
        assert!(prompt.contains("theorem thm : 2 + 2 = 4"));
        assert!(!prompt.contains("これまでの失敗"));
    }

    #[test]
    fn the_prompt_carries_the_context_and_past_failures() {
        let feedback = vec![Attempt {
            proof: "by foo".to_owned(),
            verdict: Verdict::Refuted {
                reason: "unknown tactic 'foo'".to_owned(),
            },
        }];
        let prompt = CodexSynthesizer::<ScriptedRunner>::render_prompt(
            &broken(),
            &["def f (t : ℝ) : ℝ := t".to_owned()],
            &feedback,
        );
        assert!(prompt.contains("def f"));
        assert!(prompt.contains("これまでの失敗"));
        assert!(prompt.contains("unknown tactic 'foo'"));
        assert!(prompt.contains("by foo"));
    }

    #[test]
    fn a_fenced_block_is_extracted() {
        let output = "説明します。\n```lean\nby norm_num\n```\n以上。";
        assert_eq!(
            CodexSynthesizer::<ScriptedRunner>::extract_proof(output),
            Some("by norm_num".to_owned())
        );
    }

    #[test]
    fn a_multi_line_proof_survives_extraction() {
        let output = "```lean\nby\n  intro t\n  positivity\n```";
        assert_eq!(
            CodexSynthesizer::<ScriptedRunner>::extract_proof(output),
            Some("by\n  intro t\n  positivity".to_owned())
        );
    }

    #[test]
    fn a_bare_tactic_line_is_picked_up_without_fences() {
        let output = "考えました。\nby positivity\n";
        assert_eq!(
            CodexSynthesizer::<ScriptedRunner>::extract_proof(output),
            Some("by positivity".to_owned())
        );
    }

    #[test]
    fn prose_with_no_proof_yields_nothing() {
        assert_eq!(
            CodexSynthesizer::<ScriptedRunner>::extract_proof("わかりませんでした。"),
            None
        );
    }

    #[test]
    fn the_synthesizer_pipes_the_prompt_to_codex() {
        let runner = ScriptedRunner::succeeding("```lean\nby norm_num\n```");
        let synthesizer = CodexSynthesizer::new(runner);

        let proof = synthesizer.propose(&broken(), &[], &[]);

        assert_eq!(proof, Some("by norm_num".to_owned()));
        let calls = synthesizer.runner.calls();
        assert_eq!(calls[0].0, "codex");
        assert!(calls[0].1.contains(&"exec".to_owned()));
        assert!(
            calls[0].1.contains(&"read-only".to_owned()),
            "証明を出すのに書き込み権限は要らない"
        );
        assert!(calls[0].2.contains("theorem thm"));
    }

    /// Codex が起動できなくても、落ちずに「候補なし」になる。
    #[test]
    fn a_missing_codex_yields_no_proposal_instead_of_panicking() {
        let runner = ScriptedRunner::new(vec![Err(RunError::NotFound("codex".to_owned()))]);
        let synthesizer = CodexSynthesizer::new(runner);
        assert_eq!(synthesizer.propose(&broken(), &[], &[]), None);
    }
}
