//! セルと検証義務が 1 つのグラフに載ることの統合テスト。
//!
//! `docs/検証層の設計.md` ADR-006 の中心的な主張:
//!
//! > `y = f(x)` が古くなるのと、`定理: f は単調である` が古くなるのは同じ
//! > 現象である。したがってグラフは 1 つでよい。
//!
//! これまで `ProofSpace` は `Workspace` とは別の `Kernel` を持っており、
//! **主張が実装で実現していなかった**（issue #22）。ここで確かめるのは、
//! `x = 2` というセルと、`x` に言及する定理が実際に繋がることである。

use mathrel_host::Workspace;
use mathrel_verify::{BackendId, Obligation, Trust, Verdict, Verifier};
use std::cell::RefCell;
use std::rc::Rc;

/// 呼ばれた義務を数える検証器。「再検査されなかった」を確かめるために要る。
struct Counting {
    calls: RefCell<Vec<String>>,
    context: RefCell<Vec<Vec<String>>>,
}

impl Counting {
    fn new() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            context: RefCell::new(Vec::new()),
        }
    }
}

impl Verifier for Counting {
    fn backend(&self) -> BackendId {
        BackendId::new("counting", "1")
    }
    fn verify(&self, obligation: &Obligation, context: &[String]) -> Verdict {
        self.calls.borrow_mut().push(obligation.name.clone());
        self.context.borrow_mut().push(context.to_vec());
        Verdict::Proved {
            trust: Trust::Checked,
        }
    }
}

fn calls(verifier: &Counting) -> Vec<String> {
    verifier.calls.borrow().clone()
}

/// 検証器を作り、ワークスペースに差しつつ手元にも残す。
///
/// `Rc` で共有するので、テストから呼び出し記録を覗ける。
fn with_counting(workspace: &mut Workspace) -> Rc<Counting> {
    let verifier = Rc::new(Counting::new());
    workspace.set_verifier(verifier.clone());
    verifier
}

// ---------------------------------------------------------------------
// 1 つのグラフ
// ---------------------------------------------------------------------

/// セルが提供する名前を、定理がそのまま参照できる。
#[test]
fn a_theorem_depends_on_the_cell_that_defines_its_symbol() {
    let mut workspace = Workspace::new();
    let x = workspace.add_cell("x = 2");
    let theorem = workspace.add_obligation(Obligation::new("thm", "0 ≤ x").using(&["x"]));

    let x_entity = workspace.cell(x).expect("セル").entity;
    let theorem_entity = workspace.cell(theorem).expect("義務").entity;
    let kernel = workspace.kernel();

    assert_eq!(
        kernel.dependencies(theorem_entity).expect("上流"),
        vec![x_entity],
        "定理はセルに依存する"
    );
    assert_eq!(
        kernel.dependents(x_entity).expect("下流"),
        vec![theorem_entity],
        "セルから見て定理は下流である"
    );
}

/// 評価と検証が同じループで、依存順に回る。
#[test]
fn cells_and_obligations_run_in_one_dependency_ordered_pass() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);

    let x = workspace.add_cell("x = 2");
    let y = workspace.add_cell("y = x + 1");
    let theorem = workspace.add_obligation(Obligation::new("thm", "0 ≤ y").using(&["y"]));

    let stats = workspace.evaluate();

    assert_eq!(
        stats.order,
        vec![x, y, theorem],
        "セル → セル → 定理 の順で回る"
    );
    assert_eq!(workspace.value(y).map(|v| v.render()), Some("3".to_owned()));
    assert_eq!(workspace.own_trust(theorem), Trust::Checked);
}

/// 定理どうしの引用も同じグラフに載る。
#[test]
fn one_theorem_can_cite_another() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);

    let lemma = workspace.add_obligation(Obligation::new("lemma", "P"));
    let theorem = workspace.add_obligation(Obligation::new("thm", "Q").citing(&["lemma"]));
    workspace.evaluate();

    let kernel = workspace.kernel();
    assert_eq!(
        kernel
            .dependencies(workspace.cell(theorem).expect("義務").entity)
            .expect("上流"),
        vec![workspace.cell(lemma).expect("補題").entity]
    );
}

/// 存在しない名前を参照した定理は、未解決のまま残る。エラーにはしない。
#[test]
fn an_obligation_referring_to_a_missing_cell_stays_unresolved() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);

    let theorem = workspace.add_obligation(Obligation::new("thm", "0 ≤ z").using(&["z"]));
    workspace.evaluate();

    let entity = workspace.cell(theorem).expect("義務").entity;
    assert!(workspace
        .kernel()
        .resolution(entity)
        .expect("解決状態")
        .is_unresolved());
    assert_eq!(workspace.own_trust(theorem), Trust::Unknown);

    // 後からセルを足せば解決して検査される。
    workspace.add_cell("z = 5");
    workspace.evaluate();
    assert_eq!(workspace.own_trust(theorem), Trust::Checked);
}

// ---------------------------------------------------------------------
// 早期カットオフ（証明でこそ本質的）
// ---------------------------------------------------------------------

/// セルの値が変われば、依存する定理は再検査される。
#[test]
fn changing_a_cell_rechecks_the_theorems_that_depend_on_it() {
    let mut workspace = Workspace::new();
    let verifier = with_counting(&mut workspace);

    workspace.add_cell("x = 2");
    workspace.add_obligation(Obligation::new("thm", "0 ≤ x").using(&["x"]));
    workspace.evaluate();
    assert_eq!(calls(&verifier), vec!["thm".to_owned()]);
    verifier.calls.borrow_mut().clear();

    workspace.update_cell(1, "x = 3").expect("update");
    workspace.evaluate();

    assert_eq!(calls(&verifier), vec!["thm".to_owned()], "再検査される");
}

/// **セルを書き換えても、値が変わらなければ定理は再検査されない。**
///
/// Lean の再検査は秒〜分かかる。この性質がないと、セルを触るたびに全証明を
/// 検査し直すことになり、実用にならない。
#[test]
fn rewriting_a_cell_to_the_same_value_does_not_recheck_theorems() {
    let mut workspace = Workspace::new();
    let verifier = with_counting(&mut workspace);

    workspace.add_cell("x = 2");
    let y = workspace.add_cell("y = x + 1");
    workspace.add_obligation(Obligation::new("thm", "0 ≤ y").using(&["y"]));
    workspace.evaluate();
    verifier.calls.borrow_mut().clear();

    // 書き方は変えるが、値は変わらない。
    workspace.update_cell(1, "x = 1 + 1").expect("update");
    let stats = workspace.evaluate();

    assert_eq!(
        workspace.value(y).map(|v| v.render()),
        Some("3".to_owned()),
        "y の値は変わらない"
    );
    assert!(
        calls(&verifier).is_empty(),
        "定理は再検査されない: {:?}",
        calls(&verifier)
    );
    assert!(!stats.order.contains(&3), "義務は評価対象に入らない");
}

// ---------------------------------------------------------------------
// 信頼度の伝播
// ---------------------------------------------------------------------

/// 検査済みの定理でも、仮置きの補題に依存していれば実効信頼度は上がらない。
#[test]
fn a_checked_theorem_is_only_as_strong_as_its_weakest_lemma() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);

    let lemma = workspace.add_assumption(Obligation::new("lemma", "難しい命題"));
    let theorem = workspace.add_obligation(Obligation::new("thm", "Q").citing(&["lemma"]));
    workspace.evaluate();

    assert_eq!(workspace.own_trust(lemma), Trust::Assumed);
    assert_eq!(
        workspace.own_trust(theorem),
        Trust::Checked,
        "自分は検査済み"
    );
    assert_eq!(
        workspace.effective_trust(theorem),
        Trust::Assumed,
        "しかし仮置きに依存している"
    );
    assert_eq!(workspace.weakest_link(), Trust::Assumed);
}

/// 数式セルは主張ではないので、信頼度を引き下げない。
#[test]
fn expression_cells_do_not_drag_the_trust_down() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);

    workspace.add_cell("x = 2");
    let theorem = workspace.add_obligation(Obligation::new("thm", "0 ≤ x").using(&["x"]));
    workspace.evaluate();

    assert_eq!(workspace.effective_trust(theorem), Trust::Checked);
    assert_eq!(workspace.weakest_link(), Trust::Checked);
}

/// 検証器を設定していなければ、義務は「検証器なし」として残る。
/// **「反証された」ではない。** 道具がないことと、偽であることは別である。
#[test]
fn without_a_verifier_obligations_are_unavailable_not_refuted() {
    let mut workspace = Workspace::new();
    let theorem = workspace.add_obligation(Obligation::new("thm", "P"));
    workspace.evaluate();

    let verdict = workspace.verdict(theorem).expect("判定");
    assert!(verdict.is_unavailable(), "{verdict:?}");
    assert!(!verdict.is_proved());
    assert_eq!(workspace.own_trust(theorem), Trust::Unknown);
}

// ---------------------------------------------------------------------
// 検証に渡す文脈
// ---------------------------------------------------------------------

/// 引用した補題は `axiom` として文脈に入る。
///
/// 数式セルは入らない。セルは mathrel の構文であって Lean の構文ではなく、
/// 変換は段階 2 の課題だからである（`docs/検証層の設計.md` §6、issue #27）。
/// **依存は追跡するが、翻訳はしない。**
#[test]
fn the_context_carries_cited_lemmas_as_axioms_but_not_expression_cells() {
    let mut workspace = Workspace::new();
    let verifier = with_counting(&mut workspace);

    workspace.add_cell("x = 2");
    workspace.add_obligation(Obligation::new("lemma", "0 ≤ x").using(&["x"]));
    workspace.add_obligation(
        Obligation::new("thm", "0 ≤ x + 1")
            .using(&["x"])
            .citing(&["lemma"]),
    );
    workspace.evaluate();

    let seen = verifier.context.borrow().clone();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].is_empty(), "補題の文脈は空: {:?}", seen[0]);
    assert_eq!(
        seen[1],
        vec!["axiom lemma : 0 ≤ x".to_owned()],
        "定理の文脈には引用した補題だけが入る"
    );
}

/// 循環した引用は検出され、検証から外れる。
#[test]
fn circular_citations_are_detected_and_excluded() {
    let mut workspace = Workspace::new();
    let verifier = with_counting(&mut workspace);

    let a = workspace.add_obligation(Obligation::new("a", "P").citing(&["b"]));
    workspace.add_obligation(Obligation::new("b", "Q").citing(&["a"]));
    workspace.evaluate();

    let entity = workspace.cell(a).expect("義務").entity;
    assert!(workspace.kernel().in_cycle(entity).expect("循環判定"));
    assert!(calls(&verifier).is_empty(), "循環は検証しない");
}

// ---------------------------------------------------------------------
// 書き出し
// ---------------------------------------------------------------------

/// UI 向けの JSON に信頼度が出る。
#[test]
fn the_json_snapshot_carries_trust() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);

    workspace.add_assumption(Obligation::new("lemma", "難しい命題"));
    workspace.add_obligation(Obligation::new("thm", "Q").citing(&["lemma"]));
    workspace.evaluate();

    let json = workspace.to_json();
    assert!(json.contains("\"kind\":\"obligation\""), "{json}");
    assert!(json.contains("\"trust\":\"Checked\""), "{json}");
    assert!(json.contains("\"effectiveTrust\":\"Assumed\""), "{json}");
    assert!(json.contains("\"weakestLink\":\"Assumed\""), "{json}");
}

// ---------------------------------------------------------------------
// 弱い環の名指し
// ---------------------------------------------------------------------
//
// 「全体の信頼度は Assumed です」とだけ言われても、利用者は何をすればよいか
// 分からない。**どのセルが原因で、なぜそうなったか**まで出す必要がある。

/// 満点でないものが 1 つもなければ、警告は空である。
#[test]
fn a_fully_checked_workspace_has_no_weak_links() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);
    workspace.add_cell("x = 2");
    workspace.add_obligation(Obligation::new("thm", "0 ≤ x").using(&["x"]));
    workspace.evaluate();

    assert!(workspace.weak_links().is_empty());
    assert_eq!(workspace.weakest_link(), Trust::Checked);
}

/// 仮置きそのものが弱い環として挙がる。原因は自分自身。
#[test]
fn an_assumption_names_itself_as_the_culprit() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);
    let lemma = workspace.add_assumption(Obligation::new("lemma", "難しい命題"));
    workspace.evaluate();

    let weak = workspace.weak_links();
    assert_eq!(weak.len(), 1);
    assert_eq!(weak[0].id, lemma);
    assert_eq!(weak[0].culprit, lemma, "原因は自分自身");
    assert_eq!(weak[0].effective, Trust::Assumed);
    assert!(weak[0].reason.contains("検査"), "{}", weak[0].reason);
}

/// 検査済みの定理は、**引き下げている上流を名指しする**。
#[test]
fn a_checked_theorem_names_the_upstream_that_drags_it_down() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);
    let lemma = workspace.add_assumption(Obligation::new("lemma", "難しい命題"));
    let theorem = workspace.add_obligation(Obligation::new("thm", "Q").citing(&["lemma"]));
    workspace.evaluate();

    let weak = workspace.weak_links();
    let entry = weak.iter().find(|w| w.id == theorem).expect("定理");
    assert_eq!(entry.own, Trust::Checked, "自分は検査済み");
    assert_eq!(entry.effective, Trust::Assumed);
    assert_eq!(entry.culprit, lemma, "原因は引用先の補題");
}

/// 検証器がなければ「検証器がない」と言う。「反証された」ではない。
#[test]
fn a_missing_verifier_is_reported_as_such() {
    let mut workspace = Workspace::new();
    let theorem = workspace.add_obligation(Obligation::new("thm", "P"));
    workspace.evaluate();

    let weak = workspace.weak_links();
    assert_eq!(weak.len(), 1);
    assert_eq!(weak[0].id, theorem);
    assert!(weak[0].reason.contains("検証器"), "{}", weak[0].reason);
    assert!(!weak[0].reason.contains("反証"), "{}", weak[0].reason);
}

/// 未解決の参照は、検証が通らなかったのとは違う理由として出す。
#[test]
fn an_unresolved_reference_is_its_own_reason() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);
    workspace.add_obligation(Obligation::new("thm", "0 ≤ z").using(&["z"]));
    workspace.evaluate();

    let weak = workspace.weak_links();
    assert_eq!(weak.len(), 1);
    assert!(weak[0].reason.contains("未解決"), "{}", weak[0].reason);
}

/// 循環した引用も理由として名指しする。
#[test]
fn a_citation_cycle_is_its_own_reason() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);
    workspace.add_obligation(Obligation::new("a", "P").citing(&["b"]));
    workspace.add_obligation(Obligation::new("b", "Q").citing(&["a"]));
    workspace.evaluate();

    let weak = workspace.weak_links();
    assert_eq!(weak.len(), 2);
    assert!(weak.iter().all(|w| w.reason.contains("循環")), "{weak:?}");
}

/// 仮置きを本物の検証に回すと、弱い環が消える。
#[test]
fn promoting_an_assumption_clears_the_weak_link() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);
    let lemma = workspace.add_assumption(Obligation::new("lemma", "P"));
    workspace.add_obligation(Obligation::new("thm", "Q").citing(&["lemma"]));
    workspace.evaluate();
    assert_eq!(workspace.weak_links().len(), 2);

    assert!(workspace.promote_assumption(lemma));
    workspace.evaluate();

    assert!(workspace.weak_links().is_empty());
    assert_eq!(workspace.weakest_link(), Trust::Checked);
}

/// JSON にも弱い環が出る。UI はこれを読んで警告を出す。
#[test]
fn the_json_snapshot_carries_the_weak_links() {
    let mut workspace = Workspace::new();
    with_counting(&mut workspace);
    workspace.add_assumption(Obligation::new("lemma", "難しい命題"));
    workspace.add_obligation(Obligation::new("thm", "Q").citing(&["lemma"]));
    workspace.evaluate();

    let json = workspace.to_json();
    assert!(json.contains("\"weakLinks\":["), "{json}");
    assert!(json.contains("\"culprit\":1"), "{json}");
    assert!(json.contains("\"reason\":\""), "{json}");
}
