//! ワークスペースの状態を、画面が必要とする形に写したもの。
//!
//! ここに DOM は出てこない。**描画から切り離してあるので、ブラウザなしで
//! テストできる**（`cargo test -p mathrel-ui`）。UI のバグの大半は「何を
//! 見せるか」の判断ミスであって、要素の組み立てではない。判断はここに集める。

use mathrel_host::{CellId, CellKind, Workspace};
use mathrel_verify::Trust;

/// セル 1 つの見え方。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CellView {
    /// 識別子。
    pub id: CellId,
    /// 入力された原文字列。
    pub source: String,
    /// 検証義務か。
    pub is_obligation: bool,
    /// 表示すべき値。無ければ `None`。
    pub value: Option<String>,
    /// 状態の見出しと調子。
    pub status: Status,
    /// このセル 1 つの信頼度。
    pub trust: Trust,
    /// 上流を畳み込んだ信頼度。**利用者に見せるべきはこちら。**
    pub effective_trust: Trust,
    /// 依存先のセル番号。
    pub dependencies: Vec<CellId>,
    /// 依存元のセル番号。
    pub dependents: Vec<CellId>,
    /// 直近の操作で再計算されたか。
    pub recomputed: bool,
}

impl CellView {
    /// 信頼度を 1 行で。上流に引きずられている場合はそれも出す。
    #[must_use]
    pub fn trust_label(&self) -> String {
        if self.trust == self.effective_trust {
            self.trust.label().to_owned()
        } else {
            format!("{} → 実効 {}", self.trust.label(), self.effective_trust)
        }
    }
}

/// セルの状態。**優先順位が意味を持つ。**
///
/// 「循環している」「参照が見つからない」は、値や鮮度より先に伝えるべき
/// 情報である。値が出ていないことより、なぜ出ていないかが知りたい。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Status {
    /// 表示の調子。CSS のクラス名になる。
    pub tone: Tone,
    /// 短い見出し。
    pub label: String,
    /// 補足。無ければ空。
    pub detail: String,
}

/// 状態の調子。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// 最新。
    Clean,
    /// 再計算が要る。
    Stale,
    /// まだ計算していない。
    Pending,
    /// 参照が見つからない。
    Unresolved,
    /// 参照先が複数ある。
    Ambiguous,
    /// 依存が循環している。
    Cycle,
    /// 計算や検証が失敗した。
    Error,
}

impl Tone {
    /// CSS のクラス名。
    #[must_use]
    pub fn class(self) -> &'static str {
        match self {
            Tone::Clean => "clean",
            Tone::Stale => "stale",
            Tone::Pending => "pending",
            Tone::Unresolved => "unresolved",
            Tone::Ambiguous => "ambiguous",
            Tone::Cycle => "cycle",
            Tone::Error => "error",
        }
    }
}

/// 満点でない信頼度の内訳。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WeakLinkView {
    /// 対象のセル。
    pub id: CellId,
    /// 引き下げているセル。`id` と同じこともある。
    pub culprit: CellId,
    /// 実効信頼度。
    pub effective: Trust,
    /// なぜ満点にならないのか。
    pub reason: String,
}

/// 画面全体の見え方。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SpaceView {
    /// カーネルのリビジョン。
    pub revision: u64,
    /// セル一覧。
    pub cells: Vec<CellView>,
    /// 全体で最も弱い実効信頼度。
    pub weakest_link: Trust,
    /// 満点でないものの内訳。
    pub weak_links: Vec<WeakLinkView>,
}

impl SpaceView {
    /// ワークスペースから写し取る。
    ///
    /// `recomputed` には、直近の評価で実際に再計算されたセルを渡す。**これを
    /// 見せることがこの道具の主張そのもの**であり、「1 つ変えたら何が古く
    /// なったか」を目で確かめられる唯一の場所である。
    #[must_use]
    pub fn of(workspace: &Workspace, recomputed: &[CellId]) -> Self {
        let cells = workspace
            .cells()
            .iter()
            .map(|cell| CellView {
                id: cell.id,
                source: cell.source.clone(),
                is_obligation: cell.is_obligation(),
                value: workspace.value(cell.id).map(|value| value.render()),
                status: status_of(workspace, cell.id),
                trust: workspace.own_trust(cell.id),
                effective_trust: workspace.effective_trust(cell.id),
                dependencies: ids_of(workspace, &kernel_deps(workspace, cell.id, true)),
                dependents: ids_of(workspace, &kernel_deps(workspace, cell.id, false)),
                recomputed: recomputed.contains(&cell.id),
            })
            .collect();

        Self {
            revision: workspace.kernel().revision().0,
            cells,
            weakest_link: workspace.weakest_link(),
            weak_links: workspace
                .weak_links()
                .into_iter()
                .map(|weak| WeakLinkView {
                    id: weak.id,
                    culprit: weak.culprit,
                    effective: weak.effective,
                    reason: weak.reason,
                })
                .collect(),
        }
    }

    /// 満点でないものがあるか。
    #[must_use]
    pub fn has_warning(&self) -> bool {
        !self.weak_links.is_empty()
    }
}

fn kernel_deps(workspace: &Workspace, id: CellId, upstream: bool) -> Vec<mathrel_kernel::Entity> {
    let cell = match workspace.cell(id) {
        Some(cell) => cell,
        None => return Vec::new(),
    };
    let kernel = workspace.kernel();
    if upstream {
        kernel.dependencies(cell.entity).unwrap_or_default()
    } else {
        kernel.dependents(cell.entity).unwrap_or_default()
    }
}

fn ids_of(workspace: &Workspace, entities: &[mathrel_kernel::Entity]) -> Vec<CellId> {
    entities
        .iter()
        .filter_map(|entity| {
            workspace
                .cells()
                .iter()
                .find(|cell| cell.entity == *entity)
                .map(|cell| cell.id)
        })
        .collect()
}

/// セルの状態を 1 つに畳む。
fn status_of(workspace: &Workspace, id: CellId) -> Status {
    let cell = match workspace.cell(id) {
        Some(cell) => cell,
        None => {
            return Status {
                tone: Tone::Error,
                label: "不明".to_owned(),
                detail: String::new(),
            }
        }
    };
    let kernel = workspace.kernel();

    // 循環と未解決は、値や鮮度より先に伝える。
    if kernel.in_cycle(cell.entity).unwrap_or(false) {
        return Status {
            tone: Tone::Cycle,
            label: "循環".to_owned(),
            detail: "依存が循環しているので計算できません".to_owned(),
        };
    }
    if let Ok(resolution) = kernel.resolution(cell.entity) {
        if resolution.is_unresolved() {
            let missing: Vec<String> = resolution
                .missing()
                .iter()
                .map(|capability| kernel.capability_label(*capability))
                .collect();
            return Status {
                tone: Tone::Unresolved,
                label: "未解決".to_owned(),
                detail: format!("{} が見つかりません", missing.join(", ")),
            };
        }
    }

    let failure = kernel
        .failure(cell.entity)
        .ok()
        .flatten()
        .map(str::to_owned)
        .or_else(|| cell.parse_error.clone());
    if let Some(reason) = failure {
        return Status {
            tone: Tone::Error,
            label: if cell.is_obligation() {
                "通らない".to_owned()
            } else {
                "エラー".to_owned()
            },
            detail: reason,
        };
    }

    if let Ok(resolution) = kernel.resolution(cell.entity) {
        if resolution.is_ambiguous() {
            let conflicts: Vec<String> = resolution
                .conflicts()
                .iter()
                .map(|(capability, _)| kernel.capability_label(*capability))
                .collect();
            return Status {
                tone: Tone::Ambiguous,
                label: "曖昧".to_owned(),
                detail: format!("{} の定義が複数あります", conflicts.join(", ")),
            };
        }
    }

    match kernel.freshness(cell.entity) {
        Ok(mathrel_kernel::Freshness::Clean { .. }) => Status {
            tone: Tone::Clean,
            label: if cell.is_obligation() {
                match &cell.kind {
                    CellKind::Obligation { assumed: true, .. } => "仮置き".to_owned(),
                    _ => "検証済み".to_owned(),
                }
            } else {
                "最新".to_owned()
            },
            detail: String::new(),
        },
        Ok(mathrel_kernel::Freshness::Dirty) => Status {
            tone: Tone::Stale,
            label: "要再計算".to_owned(),
            detail: "上流が変わりました".to_owned(),
        },
        Ok(mathrel_kernel::Freshness::MaybeDirty) => Status {
            tone: Tone::Stale,
            label: "要確認".to_owned(),
            detail: "上流が変わった可能性があります".to_owned(),
        },
        _ => Status {
            tone: Tone::Pending,
            label: "未計算".to_owned(),
            detail: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mathrel_host::parse_obligation_line;
    use mathrel_verify::TrivialVerifier;
    use std::rc::Rc;

    fn workspace() -> Workspace {
        let mut workspace = Workspace::new();
        workspace.set_verifier(Rc::new(TrivialVerifier));
        workspace
    }

    #[test]
    fn a_clean_cell_shows_its_value() {
        let mut workspace = workspace();
        workspace.add_cell("x = 2");
        let stats = workspace.evaluate();

        let view = SpaceView::of(&workspace, &stats.order);
        assert_eq!(view.cells[0].value.as_deref(), Some("2"));
        assert_eq!(view.cells[0].status.tone, Tone::Clean);
        assert!(view.cells[0].recomputed, "初回は再計算された扱い");
        assert!(!view.has_warning());
    }

    /// 1 つ変えたら何が再計算されたかが見える。これがこの道具の主張である。
    #[test]
    fn only_the_affected_cells_are_marked_as_recomputed() {
        let mut workspace = workspace();
        workspace.add_cell("x = 2");
        workspace.add_cell("f(t) = t^2 + 1");
        workspace.add_cell("y = f(x)");
        workspace.evaluate();

        workspace.update_cell(1, "x = 3").expect("update");
        let stats = workspace.evaluate();
        let view = SpaceView::of(&workspace, &stats.order);

        assert!(view.cells[0].recomputed, "x");
        assert!(!view.cells[1].recomputed, "f は触っていない");
        assert!(view.cells[2].recomputed, "y");
        assert_eq!(view.cells[2].value.as_deref(), Some("10"));
    }

    /// 循環は値や鮮度より先に伝える。
    #[test]
    fn a_cycle_outranks_everything_else() {
        let mut workspace = workspace();
        workspace.add_cell("a = b + 1");
        workspace.add_cell("b = a + 1");
        workspace.evaluate();

        let view = SpaceView::of(&workspace, &[]);
        assert_eq!(view.cells[0].status.tone, Tone::Cycle);
    }

    #[test]
    fn an_unresolved_reference_names_what_is_missing() {
        let mut workspace = workspace();
        workspace.add_cell("y = z + 1");
        workspace.evaluate();

        let view = SpaceView::of(&workspace, &[]);
        assert_eq!(view.cells[0].status.tone, Tone::Unresolved);
        assert!(view.cells[0].status.detail.contains("NameBound(z)"));
    }

    #[test]
    fn a_parse_error_is_shown_as_an_error() {
        let mut workspace = workspace();
        workspace.add_cell("a = 1 +");
        workspace.evaluate();

        let view = SpaceView::of(&workspace, &[]);
        assert_eq!(view.cells[0].status.tone, Tone::Error);
        assert!(!view.cells[0].status.detail.is_empty());
    }

    #[test]
    fn dependencies_are_shown_in_both_directions() {
        let mut workspace = workspace();
        workspace.add_cell("x = 2");
        workspace.add_cell("y = x + 1");
        workspace.evaluate();

        let view = SpaceView::of(&workspace, &[]);
        assert_eq!(view.cells[1].dependencies, vec![1]);
        assert_eq!(view.cells[0].dependents, vec![2]);
    }

    /// 満点でないものは、原因つきで警告になる。
    #[test]
    fn a_weak_link_is_reported_with_its_culprit() {
        let mut workspace = workspace();
        let lemma =
            workspace.add_assumption(parse_obligation_line("lemma : 難しい命題").expect("parse"));
        let theorem = workspace
            .add_obligation(parse_obligation_line("thm : a = a cites lemma").expect("parse"));
        workspace.evaluate();

        let view = SpaceView::of(&workspace, &[]);
        assert!(view.has_warning());
        assert_eq!(view.weakest_link, Trust::Assumed);

        let entry = view
            .weak_links
            .iter()
            .find(|w| w.id == theorem)
            .expect("定理");
        assert_eq!(entry.culprit, lemma, "引き下げている上流を名指しする");
        assert_eq!(entry.effective, Trust::Assumed);
        assert!(entry.reason.contains("検査"), "{}", entry.reason);
    }

    /// 検査済みでも上流に引きずられていることが 1 行で分かる。
    #[test]
    fn the_trust_label_shows_when_it_is_dragged_down() {
        let mut workspace = workspace();
        workspace.add_assumption(parse_obligation_line("lemma : 難しい命題").expect("parse"));
        workspace.add_obligation(parse_obligation_line("thm : a = a cites lemma").expect("parse"));
        workspace.evaluate();

        let view = SpaceView::of(&workspace, &[]);
        assert_eq!(view.cells[0].trust_label(), "Assumed");
        assert_eq!(view.cells[1].trust_label(), "Checked → 実効 Assumed");
    }

    #[test]
    fn an_obligation_is_marked_as_one() {
        let mut workspace = workspace();
        workspace.add_cell("x = 2");
        workspace.add_obligation(parse_obligation_line("thm : a = a").expect("parse"));
        workspace.evaluate();

        let view = SpaceView::of(&workspace, &[]);
        assert!(!view.cells[0].is_obligation);
        assert!(view.cells[1].is_obligation);
        assert_eq!(view.cells[1].status.label, "検証済み");
    }

    #[test]
    fn an_assumption_says_it_is_a_placeholder() {
        let mut workspace = workspace();
        workspace.add_assumption(parse_obligation_line("hard : 難しい命題").expect("parse"));
        workspace.evaluate();

        let view = SpaceView::of(&workspace, &[]);
        assert_eq!(view.cells[0].status.label, "仮置き");
    }
}
