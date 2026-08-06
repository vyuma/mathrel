//! セッション — 入力 1 行を受けて、出力の行を返す。
//!
//! 端末との入出力は [`crate::main`] が行い、ここは純粋な関数として保つ。
//! そうしないと CLI の振る舞いをテストできない。

use mathrel_host::Workspace;
use mathrel_kernel::Freshness;
use mathrel_verify::{Obligation, ProofSpace, TrivialVerifier, Trust};

/// 1 行の入力に対する応答。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Response {
    /// 表示する行。
    pub lines: Vec<String>,
    /// 終了するか。
    pub quit: bool,
}

impl Response {
    fn of(lines: Vec<String>) -> Self {
        Self { lines, quit: false }
    }

    fn line(text: impl Into<String>) -> Self {
        Self::of(vec![text.into()])
    }

    fn empty() -> Self {
        Self::default()
    }

    /// 出力を 1 つの文字列にする。テストが使う。
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// CLI の状態。
pub struct Session {
    workspace: Workspace,
    proofs: ProofSpace,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// 空のセッション。
    #[must_use]
    pub fn new() -> Self {
        Self {
            workspace: Workspace::new(),
            proofs: ProofSpace::new(),
        }
    }

    /// 1 行を処理する。
    pub fn handle(&mut self, line: &str) -> Response {
        let line = line.trim();
        if line.is_empty() {
            return Response::empty();
        }
        if !line.starts_with(':') {
            return self.add(line);
        }

        let (command, rest) = split_once_trimmed(line);
        match command {
            ":help" | ":h" | ":?" => Response::of(help()),
            ":quit" | ":q" => Response {
                lines: vec!["さようなら".to_owned()],
                quit: true,
            },
            ":list" | ":l" => Response::of(self.list()),
            ":set" => self.set(rest),
            ":del" => self.delete(rest),
            ":deps" => self.deps(rest),
            ":json" => Response::line(self.workspace.to_json()),
            ":thm" => self.add_obligation(rest, false),
            ":assume" => self.add_obligation(rest, true),
            ":verify" => Response::of(self.verify()),
            ":demo" => Response::of(self.demo()),
            other => Response::line(format!("知らない命令です: {other}（:help で一覧）")),
        }
    }

    // ---------------------------------------------------------------
    // 評価
    // ---------------------------------------------------------------

    fn add(&mut self, source: &str) -> Response {
        let id = self.workspace.add_cell(source);
        let stats = self.workspace.evaluate();
        let mut lines = vec![self.render_cell(id)];
        if let Some(note) = self.recompute_note(&stats.order, id) {
            lines.push(note);
        }
        Response::of(lines)
    }

    fn set(&mut self, rest: &str) -> Response {
        let (id, source) = split_once_trimmed(rest);
        let id = match id.parse::<u64>() {
            Ok(id) => id,
            Err(_) => return Response::line(":set <番号> <式> の形で指定してください"),
        };
        if source.is_empty() {
            return Response::line(":set <番号> <式> の形で指定してください");
        }
        if let Err(error) = self.workspace.update_cell(id, source) {
            return Response::line(error.to_string());
        }

        let stats = self.workspace.evaluate();
        let mut lines = vec![self.render_cell(id)];
        if let Some(note) = self.recompute_note(&stats.order, id) {
            lines.push(note);
        }
        Response::of(lines)
    }

    fn delete(&mut self, rest: &str) -> Response {
        let id = match rest.trim().parse::<u64>() {
            Ok(id) => id,
            Err(_) => return Response::line(":del <番号> の形で指定してください"),
        };
        match self.workspace.remove_cell(id) {
            Ok(()) => {
                self.workspace.evaluate();
                Response::line(format!("[{id}] を削除しました"))
            }
            Err(error) => Response::line(error.to_string()),
        }
    }

    fn deps(&self, rest: &str) -> Response {
        let id = match rest.trim().parse::<u64>() {
            Ok(id) => id,
            Err(_) => return Response::line(":deps <番号> の形で指定してください"),
        };
        let cell = match self.workspace.cell(id) {
            Some(cell) => cell,
            None => return Response::line(format!("セル {id} がありません")),
        };
        let kernel = self.workspace.kernel();

        let upstream = self.ids_of(&kernel.dependencies(cell.entity).unwrap_or_default());
        let downstream = self.ids_of(&kernel.dependents(cell.entity).unwrap_or_default());
        let requires: Vec<String> = kernel
            .requires(cell.entity)
            .iter()
            .map(|capability| kernel.capability_label(*capability))
            .collect();
        let provides: Vec<String> = kernel
            .provides(cell.entity)
            .iter()
            .map(|capability| kernel.capability_label(*capability))
            .collect();

        Response::of(vec![
            format!("[{id}] {}", cell.source),
            format!("  provides : {}", join_or_dash(&provides)),
            format!("  requires : {}", join_or_dash(&requires)),
            format!("  依存先   : {}", join_or_dash(&upstream)),
            format!("  依存元   : {}", join_or_dash(&downstream)),
        ])
    }

    fn list(&self) -> Vec<String> {
        if self.workspace.cells().is_empty() {
            return vec!["セルがありません".to_owned()];
        }
        self.workspace
            .cells()
            .iter()
            .map(|cell| self.render_cell(cell.id))
            .collect()
    }

    fn render_cell(&self, id: u64) -> String {
        let cell = match self.workspace.cell(id) {
            Some(cell) => cell,
            None => return format!("[{id}] （ありません）"),
        };
        let kernel = self.workspace.kernel();
        let state = kernel
            .freshness(cell.entity)
            .map(Freshness::kind_str)
            .unwrap_or("?");

        let mut line = format!("[{id}] {}", cell.source);
        if let Some(value) = self.workspace.value(id) {
            line.push_str(&format!("  → {}", value.render()));
        }

        let mut notes: Vec<String> = vec![state.to_owned()];
        if kernel.in_cycle(cell.entity).unwrap_or(false) {
            notes.push("循環".to_owned());
        }
        if let Ok(resolution) = kernel.resolution(cell.entity) {
            if resolution.is_unresolved() {
                let missing: Vec<String> = resolution
                    .missing()
                    .iter()
                    .map(|capability| kernel.capability_label(*capability))
                    .collect();
                notes.push(format!("未解決: {}", missing.join(", ")));
            }
            if resolution.is_ambiguous() {
                notes.push("曖昧".to_owned());
            }
        }
        if let Some(error) = kernel
            .failure(cell.entity)
            .ok()
            .flatten()
            .or(cell.parse_error.as_deref())
        {
            notes.push(format!("エラー: {error}"));
        }
        line.push_str(&format!("  ({})", notes.join(" / ")));
        line
    }

    /// 「何が再計算されたか」を出す。P1 の完了条件がこれである。
    fn recompute_note(&self, order: &[u64], just_edited: u64) -> Option<String> {
        let others: Vec<String> = order
            .iter()
            .filter(|id| **id != just_edited)
            .map(u64::to_string)
            .collect();
        if others.is_empty() {
            return None;
        }
        Some(format!("  再計算: [{}]", others.join("], [")))
    }

    fn ids_of(&self, entities: &[mathrel_kernel::Entity]) -> Vec<String> {
        entities
            .iter()
            .filter_map(|entity| {
                self.workspace
                    .cells()
                    .iter()
                    .find(|cell| cell.entity == *entity)
                    .map(|cell| format!("[{}]", cell.id))
            })
            .collect()
    }

    // ---------------------------------------------------------------
    // 検証
    // ---------------------------------------------------------------

    /// `:thm 名前 : 命題` / `:assume 名前 : 命題`
    ///
    /// NOTE: 検証空間は今のところ評価のワークスペースとは別のグラフである。
    /// 1 つに統合するのは企画書 P1.5（`docs/検証層の設計.md` §7）。
    fn add_obligation(&mut self, rest: &str, assume: bool) -> Response {
        let (name, statement) = match rest.split_once(':') {
            Some((name, statement)) => (name.trim(), statement.trim()),
            None => {
                return Response::line(if assume {
                    ":assume <名前> : <命題> の形で指定してください"
                } else {
                    ":thm <名前> : <命題> の形で指定してください"
                })
            }
        };
        if name.is_empty() || statement.is_empty() {
            return Response::line("名前と命題の両方が要ります");
        }

        let obligation = Obligation::new(name, statement);
        let entity = if assume {
            self.proofs.add_assumption(obligation)
        } else {
            self.proofs.add_obligation(obligation)
        };
        self.proofs.verify_all(&TrivialVerifier);

        Response::line(format!(
            "{name} : {statement}  ({})",
            self.render_trust(entity)
        ))
    }

    fn verify(&mut self) -> Vec<String> {
        if self.proofs.entries().is_empty() {
            return vec!["検証義務がありません（:thm で登録します）".to_owned()];
        }
        let stats = self.proofs.verify_all(&TrivialVerifier);
        let mut lines = vec![if stats.verified.is_empty() {
            // 古くなったものが 1 つもなければ、検証器は 1 度も呼ばれない。
            // これは異常ではなく、早期カットオフが効いている状態である。
            "再検査は不要でした（前回の判定がそのまま有効）".to_owned()
        } else {
            format!(
                "再検査 {} 件: 検査済み {} / 仮置き {} / 不成立 {} / 検証器なし {}",
                stats.verified.len(),
                stats.checked,
                stats.assumed,
                stats.failed,
                stats.unavailable
            )
        }];
        for entry in self.proofs.entries() {
            lines.push(format!(
                "  {} : {}",
                entry.name,
                self.render_trust(entry.entity)
            ));
        }
        lines.push(format!(
            "全体の信頼度（最も弱い環）: {}",
            self.proofs.weakest_link()
        ));
        lines
    }

    fn render_trust(&self, entity: mathrel_kernel::Entity) -> String {
        let own = self.proofs.own_trust(entity);
        let effective = self.proofs.effective_trust(entity);
        if own == effective {
            own.label().to_owned()
        } else {
            format!("{} → 実効 {}", own.label(), effective.label())
        }
    }

    // ---------------------------------------------------------------
    // デモ
    // ---------------------------------------------------------------

    /// 企画書 §8 の P1 完了条件を、そのまま流す。
    pub fn demo(&mut self) -> Vec<String> {
        let mut lines = vec!["-- 企画書 P1 の完了条件 --".to_owned()];
        for source in ["x = 2", "f(t) = t^2 + 1", "y = f(x)"] {
            lines.push(format!("> {source}"));
            lines.extend(self.handle(source).lines);
        }
        lines.push("> :set 1 x = 3".to_owned());
        lines.extend(self.handle(":set 1 x = 3").lines);
        lines.push("  ↑ x だけを変えた。再計算されたのは y だけで、f は触っていない。".to_owned());
        lines
    }

    /// 直近の全体信頼度。
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn weakest_link(&self) -> Trust {
        self.proofs.weakest_link()
    }
}

fn split_once_trimmed(line: &str) -> (&str, &str) {
    match line.split_once(char::is_whitespace) {
        Some((head, rest)) => (head.trim(), rest.trim()),
        None => (line.trim(), ""),
    }
}

fn join_or_dash(items: &[String]) -> String {
    if items.is_empty() {
        "—".to_owned()
    } else {
        items.join(", ")
    }
}

fn help() -> Vec<String> {
    vec![
        "式を入力すると、そのままセルになります（例: x = 2）".to_owned(),
        "".to_owned(),
        "  :list            セル一覧".to_owned(),
        "  :set <番号> <式>  セルを書き換える".to_owned(),
        "  :del <番号>       セルを消す".to_owned(),
        "  :deps <番号>      依存関係を見る".to_owned(),
        "  :json            UI 向けの状態を JSON で出す".to_owned(),
        "  :thm <名> : <命題>    検証義務を登録して検証する".to_owned(),
        "  :assume <名> : <命題> 「既知」として登録する（検査はしない）".to_owned(),
        "  :verify          検証をやり直して信頼度を出す".to_owned(),
        "  :demo            企画書 P1 の完了条件を流す".to_owned(),
        "  :quit            終了".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(session: &mut Session, line: &str) -> String {
        session.handle(line).text()
    }

    // --- 評価 ---

    #[test]
    fn a_cell_is_added_and_evaluated() {
        let mut session = Session::new();
        let output = run(&mut session, "x = 2");
        assert!(output.contains("[1] x = 2"), "{output}");
        assert!(output.contains("→ 2"), "{output}");
        assert!(output.contains("Clean"), "{output}");
    }

    /// 企画書 §8 の P1 完了条件。
    #[test]
    fn changing_x_recomputes_only_y() {
        let mut session = Session::new();
        run(&mut session, "x = 2");
        run(&mut session, "f(t) = t^2 + 1");
        assert!(run(&mut session, "y = f(x)").contains("→ 5"));

        let output = run(&mut session, ":set 1 x = 3");
        assert!(output.contains("[1] x = 3"), "{output}");
        assert!(
            output.contains("再計算: [3]"),
            "y だけが再計算される: {output}"
        );
        assert!(!output.contains("[2]"), "f は再計算されない: {output}");
        assert!(run(&mut session, ":list").contains("→ 10"));
    }

    /// 値が変わらなければ、再計算は直接下流 1 段で止まる。
    ///
    /// カーネルは変更点の直接下流を無条件に `Dirty` にする（指紋が変わったかは
    /// 評価しないと分からないため）。2 段目より先は `MaybeDirty` に留まり、
    /// 早期カットオフが救う。詳細は `docs/検証層の設計.md` §6.1(b)。
    #[test]
    fn editing_a_cell_to_the_same_text_stops_one_step_downstream() {
        let mut session = Session::new();
        run(&mut session, "x = 2");
        run(&mut session, "y = x + 1");
        run(&mut session, "z = y + 1");

        let output = run(&mut session, ":set 1 x = 2");
        assert!(
            output.contains("再計算: [2]"),
            "直接下流は 1 回動く: {output}"
        );
        assert!(!output.contains("[3]"), "2 段目より先は動かない: {output}");
    }

    /// 値が変われば末端まで届く。
    #[test]
    fn a_real_change_reaches_the_end_of_the_chain() {
        let mut session = Session::new();
        run(&mut session, "x = 2");
        run(&mut session, "y = x + 1");
        run(&mut session, "z = y + 1");

        let output = run(&mut session, ":set 1 x = 5");
        assert!(output.contains("再計算: [2], [3]"), "{output}");
        assert!(run(&mut session, ":list").contains("→ 7"));
    }

    #[test]
    fn an_unresolved_reference_is_reported_not_crashed() {
        let mut session = Session::new();
        let output = run(&mut session, "y = z + 1");
        assert!(output.contains("未解決"), "{output}");
        assert!(output.contains("NameBound(z)"), "{output}");
    }

    /// 未解決のまま置いておき、後から埋めれば動く。
    #[test]
    fn filling_in_a_missing_definition_makes_it_evaluate() {
        let mut session = Session::new();
        run(&mut session, "y = z + 1");
        run(&mut session, "z = 10");
        assert!(run(&mut session, ":list").contains("→ 11"));
    }

    #[test]
    fn a_broken_cell_can_be_fixed() {
        let mut session = Session::new();
        let broken = run(&mut session, "a = 1 +");
        assert!(broken.contains("エラー"), "{broken}");

        let fixed = run(&mut session, ":set 1 a = 1 + 2");
        assert!(fixed.contains("→ 3"), "直せば計算される: {fixed}");
    }

    #[test]
    fn dependencies_are_shown_in_both_directions() {
        let mut session = Session::new();
        run(&mut session, "x = 2");
        run(&mut session, "y = x + 1");

        let output = run(&mut session, ":deps 2");
        assert!(output.contains("依存先   : [1]"), "{output}");
        assert!(output.contains("requires : NameBound(x)"), "{output}");

        let upstream = run(&mut session, ":deps 1");
        assert!(upstream.contains("依存元   : [2]"), "{upstream}");
    }

    #[test]
    fn deleting_a_cell_leaves_its_dependents_unresolved() {
        let mut session = Session::new();
        run(&mut session, "x = 2");
        run(&mut session, "y = x + 1");

        assert!(run(&mut session, ":del 1").contains("削除"));
        let list = run(&mut session, ":list");
        assert!(list.contains("未解決"), "y は残って未解決になる: {list}");
    }

    #[test]
    fn the_json_snapshot_is_emitted() {
        let mut session = Session::new();
        run(&mut session, "x = 2");
        let output = run(&mut session, ":json");
        assert!(output.starts_with('{'), "{output}");
        assert!(output.contains("\"revision\""), "{output}");
        assert!(output.contains("\"cells\""), "{output}");
    }

    // --- 検証 ---

    #[test]
    fn a_trivial_theorem_is_checked() {
        let mut session = Session::new();
        let output = run(&mut session, ":thm refl : f x = f x");
        assert!(output.contains("Checked"), "{output}");
    }

    #[test]
    fn an_assumption_is_recorded_as_assumed() {
        let mut session = Session::new();
        let output = run(&mut session, ":assume big : 難しい命題");
        assert!(output.contains("Assumed"), "{output}");
        assert!(!output.contains("Checked"), "{output}");
        assert_eq!(session.weakest_link(), Trust::Assumed);
    }

    #[test]
    fn the_verify_summary_reports_the_weakest_link() {
        let mut session = Session::new();
        run(&mut session, ":assume big : 難しい命題");
        run(&mut session, ":thm refl : a = a");

        let output = run(&mut session, ":verify");
        assert!(output.contains("全体の信頼度"), "{output}");
        assert!(output.contains("Assumed"), "{output}");
    }

    /// 何も古くなっていなければ、検証器は 1 度も呼ばれない。
    /// それは異常ではなく、早期カットオフが効いている状態である。
    #[test]
    fn verifying_twice_says_nothing_needed_rechecking() {
        let mut session = Session::new();
        run(&mut session, ":thm refl : a = a");

        let output = run(&mut session, ":verify");
        assert!(output.contains("再検査は不要"), "{output}");
        assert!(output.contains("Checked"), "判定は残る: {output}");
    }

    #[test]
    fn verifying_an_empty_space_says_so() {
        let mut session = Session::new();
        assert!(run(&mut session, ":verify").contains("ありません"));
    }

    // --- 入力の扱い ---

    #[test]
    fn blank_input_produces_nothing() {
        let mut session = Session::new();
        assert_eq!(session.handle("   "), Response::empty());
    }

    #[test]
    fn an_unknown_command_is_reported() {
        let mut session = Session::new();
        assert!(run(&mut session, ":nope").contains("知らない命令"));
    }

    #[test]
    fn malformed_commands_do_not_panic() {
        let mut session = Session::new();
        for line in [
            ":set",
            ":set abc",
            ":set 1",
            ":del",
            ":del x",
            ":deps",
            ":thm",
            ":assume oops",
        ] {
            let response = session.handle(line);
            assert!(!response.lines.is_empty(), "{line} に応答がない");
            assert!(!response.quit);
        }
    }

    #[test]
    fn quit_is_signalled() {
        let mut session = Session::new();
        assert!(session.handle(":quit").quit);
        assert!(session.handle(":q").quit);
        assert!(!session.handle(":list").quit);
    }

    #[test]
    fn help_lists_the_commands() {
        let mut session = Session::new();
        let output = run(&mut session, ":help");
        assert!(output.contains(":deps"));
        assert!(output.contains(":verify"));
    }

    /// `:demo` は企画書 P1 の完了条件をそのまま実演する。
    #[test]
    fn the_demo_shows_only_y_being_recomputed() {
        let mut session = Session::new();
        let output = session.demo().join("\n");
        assert!(output.contains("→ 5"), "{output}");
        assert!(output.contains("[1] x = 3"), "{output}");
        assert!(output.contains("再計算: [3]"), "{output}");
    }
}
