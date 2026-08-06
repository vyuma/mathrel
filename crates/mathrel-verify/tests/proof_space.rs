//! 検証空間の統合テスト。
//!
//! 確かめるのは 3 つ。
//!
//! 1. 定義と証明が 1 つのグラフに載り、変更が伝播すること（ADR-006）
//! 2. **証明の再検査に早期カットオフが効くこと**（設計の主目的）
//! 3. 信頼度が依存に沿って正しく落ちること（ADR-008）

use mathrel_verify::{
    BackendId, Obligation, ProofSpace, TrivialVerifier, Trust, Verdict, Verifier,
};
use std::cell::RefCell;

// ---------------------------------------------------------------------
// 補助
// ---------------------------------------------------------------------

/// 何回呼ばれたかを数える検証器。「再検査されなかった」を確かめるために要る。
struct CountingVerifier {
    inner: TrivialVerifier,
    calls: RefCell<Vec<String>>,
}

impl CountingVerifier {
    fn new() -> Self {
        Self {
            inner: TrivialVerifier,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn reset(&self) {
        self.calls.borrow_mut().clear();
    }
}

impl Verifier for CountingVerifier {
    fn backend(&self) -> BackendId {
        self.inner.backend()
    }
    fn verify(&self, obligation: &Obligation, context: &[String]) -> Verdict {
        self.calls.borrow_mut().push(obligation.name.clone());
        self.inner.verify(obligation, context)
    }
}

/// 何でも `Checked` で通す検証器。文脈の中身だけを見たいときに使う。
struct AlwaysChecked {
    seen_context: RefCell<Vec<Vec<String>>>,
}

impl AlwaysChecked {
    fn new() -> Self {
        Self {
            seen_context: RefCell::new(Vec::new()),
        }
    }
}

impl Verifier for AlwaysChecked {
    fn backend(&self) -> BackendId {
        BackendId::new("always", "1")
    }
    fn verify(&self, _obligation: &Obligation, context: &[String]) -> Verdict {
        self.seen_context.borrow_mut().push(context.to_vec());
        Verdict::Proved {
            trust: Trust::Checked,
        }
    }
}

/// 版を名乗り、呼ばれた義務を数える検証器。
struct VersionedVerifier {
    version: String,
    calls: RefCell<Vec<String>>,
}

impl VersionedVerifier {
    fn new(version: &str) -> Self {
        Self {
            version: version.to_owned(),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Verifier for VersionedVerifier {
    fn backend(&self) -> BackendId {
        BackendId::new("lean4", &self.version)
    }
    fn verify(&self, obligation: &Obligation, _context: &[String]) -> Verdict {
        self.calls.borrow_mut().push(obligation.name.clone());
        Verdict::Proved {
            trust: Trust::Checked,
        }
    }
}

/// 道具の有無だけが変わる検証器。**版は変わらない。**
///
/// 「Lean を入れた」は版の変更ではない。設定に書いた版はそのままで、実行
/// ファイルが PATH に現れただけである。したがって版の変更による一括再検査は
/// 働かず、別の出口が要る。
struct FlakyTool {
    available: std::cell::Cell<bool>,
    calls: RefCell<Vec<String>>,
}

impl FlakyTool {
    fn new(available: bool) -> Self {
        Self {
            available: std::cell::Cell::new(available),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn install(&self) {
        self.available.set(true);
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn reset(&self) {
        self.calls.borrow_mut().clear();
    }
}

impl Verifier for FlakyTool {
    fn backend(&self) -> BackendId {
        BackendId::new("lean4", "4.8.0")
    }
    fn verify(&self, obligation: &Obligation, _context: &[String]) -> Verdict {
        self.calls.borrow_mut().push(obligation.name.clone());
        if self.available.get() {
            Verdict::Proved {
                trust: Trust::Checked,
            }
        } else {
            Verdict::Unavailable {
                reason: "lean が見つかりません".to_owned(),
            }
        }
    }
}

/// `c` → `f` → 補題 → 定理 の連鎖を作る。
fn chain() -> ProofSpace {
    let mut space = ProofSpace::new();
    space.add_definition("c", "def c : Nat := 1");
    space.add_definition_using("f", "def f (t : Nat) : Nat := t + c", &["c"]);
    space.add_obligation(
        Obligation::new("f_refl", "f t = f t")
            .using(&["f"])
            .with_proof("rfl"),
    );
    space.add_obligation(
        Obligation::new("main", "f t = f t")
            .citing(&["f_refl"])
            .with_proof("rfl"),
    );
    space
}

// ---------------------------------------------------------------------
// 1. 1 つのグラフ
// ---------------------------------------------------------------------

#[test]
fn definitions_and_proofs_share_one_dependency_graph() {
    let space = chain();
    let kernel = space.kernel();

    let f = space.entry_named("f").expect("f").entity;
    let lemma = space.entry_named("f_refl").expect("補題").entity;
    let theorem = space.entry_named("main").expect("定理").entity;

    assert_eq!(kernel.dependencies(lemma).expect("上流"), vec![f]);
    assert_eq!(kernel.dependencies(theorem).expect("上流"), vec![lemma]);
    assert_eq!(kernel.dependents(lemma).expect("下流"), vec![theorem]);
}

#[test]
fn everything_gets_verified_in_dependency_order() {
    let mut space = chain();
    let verifier = CountingVerifier::new();

    let stats = space.verify_all(&verifier);

    assert_eq!(stats.checked, 2);
    assert_eq!(stats.assumed, 0);
    assert_eq!(stats.failed, 0);
    assert_eq!(
        verifier.calls(),
        vec!["f_refl".to_owned(), "main".to_owned()],
        "補題が先、定理が後"
    );
}

/// 存在しない補題を引用したら、未解決として残る。エラーにはしない。
#[test]
fn citing_an_unknown_lemma_leaves_the_theorem_unresolved() {
    let mut space = ProofSpace::new();
    let theorem = space.add_obligation(Obligation::new("main", "P").citing(&["missing_lemma"]));

    let verifier = CountingVerifier::new();
    space.verify_all(&verifier);

    assert!(space
        .kernel()
        .resolution(theorem)
        .expect("解決状態")
        .is_unresolved());
    assert!(verifier.calls().is_empty(), "検証器は呼ばれない");
    assert_eq!(space.own_trust(theorem), Trust::Unknown);
}

/// 引用が循環していたら検出され、評価から外れる。
///
/// これが検出できることが、`axiom` として引用する方式の健全性を支えている。
#[test]
fn circular_citations_are_detected_and_excluded() {
    let mut space = ProofSpace::new();
    let a = space.add_obligation(Obligation::new("a", "P").citing(&["b"]));
    let b = space.add_obligation(Obligation::new("b", "Q").citing(&["a"]));

    let verifier = CountingVerifier::new();
    space.verify_all(&verifier);

    assert!(space.kernel().in_cycle(a).expect("循環判定"));
    assert!(space.kernel().in_cycle(b).expect("循環判定"));
    assert!(verifier.calls().is_empty(), "循環したものは検証しない");
}

// ---------------------------------------------------------------------
// 2. 早期カットオフ（本題）
// ---------------------------------------------------------------------

/// 定義を書き換えても、内容が同じなら証明は 1 つも再検査されない。
///
/// Lean の再検査は秒〜分かかる。この性質がないと、定義を触るたびに全証明を
/// 検査し直すことになり、実用にならない。
#[test]
fn rewriting_a_definition_to_the_same_text_rechecks_nothing() {
    let mut space = chain();
    let verifier = CountingVerifier::new();
    space.verify_all(&verifier);
    verifier.reset();

    let c = space.entry_named("c").expect("c").entity;
    space.update_definition(c, "def c : Nat := 1"); // 中身は同じ
    let stats = space.verify_all(&verifier);

    assert!(
        verifier.calls().is_empty(),
        "証明は 1 つも再検査されない: {:?}",
        verifier.calls()
    );
    assert_eq!(stats.proved(), 0);
    assert_eq!(
        space.effective_trust(space.entry_named("main").expect("定理").entity),
        Trust::Checked
    );
}

/// 定義の中身が変われば、依存する証明は再検査される。
#[test]
fn changing_a_definition_rechecks_the_proofs_that_depend_on_it() {
    let mut space = chain();
    let verifier = CountingVerifier::new();
    space.verify_all(&verifier);
    verifier.reset();

    let c = space.entry_named("c").expect("c").entity;
    space.update_definition(c, "def c : Nat := 2");
    space.verify_all(&verifier);

    assert_eq!(
        verifier.calls(),
        vec!["f_refl".to_owned(), "main".to_owned()],
        "連鎖の末端まで再検査される"
    );
}

/// 証明を書き直しても、波及は直接の引用元 1 段で止まる。
///
/// proof irrelevance により、命題が同じなら指紋は変わらない。ただしカーネルは
/// 変更点の**直接下流を無条件に `Dirty`** にする設計なので、直接の引用元だけは
/// 1 回再検査される（指紋が変わったかは評価しないと分からないため）。2 段目
/// より先は `MaybeDirty` に留まり、早期カットオフが救う。
///
/// この 1 回分の無駄は算術なら誤差だが、Lean では秒〜分になる。改善案は
/// `docs/検証層の設計.md` §6.1(b) に「未解決」として記録してある。
#[test]
fn rewriting_a_proof_stops_one_step_downstream() {
    let mut space = chain();
    // 引用の連鎖をもう 1 段伸ばす: f_refl ← main ← outer
    space.add_obligation(
        Obligation::new("outer", "f t = f t")
            .citing(&["main"])
            .with_proof("rfl"),
    );
    let verifier = CountingVerifier::new();
    space.verify_all(&verifier);
    assert_eq!(verifier.calls().len(), 3);
    verifier.reset();

    let lemma = space.entry_named("f_refl").expect("補題").entity;
    space.set_proof(lemma, "by simp"); // 命題は変えず、証明だけ差し替える
    space.verify_all(&verifier);

    assert_eq!(
        verifier.calls(),
        vec!["f_refl".to_owned(), "main".to_owned()],
        "直接の引用元までで止まり、outer は再検査されない"
    );
}

/// 検証器が使えなかった義務は、環境が整えば再試行できる。
///
/// カーネルは失敗した項目を「入力が変わるまで」再スケジュールしない。しかし
/// Lean を入れれば、入力が同じでも結果は変わる。`retry_failed` はそのための
/// 出口である。これがないと、Lean を入れた後もワークスペース全体が
/// 「検証できませんでした」のまま固まる。
#[test]
fn a_failed_obligation_can_be_retried_after_the_tool_arrives() {
    let mut space = ProofSpace::new();
    let theorem = space.add_obligation(Obligation::new("thm", "難しい命題"));

    let tool = FlakyTool::new(false);
    space.verify_all(&tool);
    assert_eq!(space.own_trust(theorem), Trust::Unknown);

    // Lean を入れた。版は変えていないので、版による一括再検査は働かない。
    tool.install();
    tool.reset();
    space.verify_all(&tool);
    assert!(
        tool.calls().is_empty(),
        "入力も版も変わっていないので、そのままでは動かない"
    );

    // 明示的に戻せば再検査される。
    assert_eq!(space.retry_failed(), 1);
    space.verify_all(&tool);
    assert_eq!(tool.calls(), vec!["thm".to_owned()]);
    assert_eq!(space.own_trust(theorem), Trust::Checked);
}

/// 検証器の版が変われば、全証明が再検査される。
///
/// 回帰テスト。指紋に版を混ぜるだけでは足りない。カーネルは版のことを知らず、
/// `next_batch` は「古いもの」しか返さないので、全部 `Clean` のままだと
/// 何も起きない。`ProofSpace` 側で明示的に古くする必要がある。
/// 「Lean を上げたら全証明が再検査される」という設計上の主張は、これがないと
/// 成立しない。
#[test]
fn upgrading_the_backend_rechecks_everything() {
    let mut space = chain();
    let first = VersionedVerifier::new("4.8.0");
    space.verify_all(&first);
    assert_eq!(first.calls(), vec!["f_refl".to_owned(), "main".to_owned()]);

    // 同じ版なら何も起きない。
    let same = VersionedVerifier::new("4.8.0");
    space.verify_all(&same);
    assert!(same.calls().is_empty(), "版が同じなら再検査しない");

    // 版が変われば全部やり直す。
    let upgraded = VersionedVerifier::new("4.9.0+mathlib@newrev");
    space.verify_all(&upgraded);
    assert_eq!(
        upgraded.calls(),
        vec!["f_refl".to_owned(), "main".to_owned()],
        "版が変われば全証明が再検査される"
    );
}

#[test]
fn retrying_leaves_the_proved_ones_alone() {
    let mut space = chain();
    space.verify_all(&AlwaysChecked::new());
    assert_eq!(space.retry_failed(), 0, "通っているものは戻さない");
}

/// 統計は検査済みと仮置きを分けて数える。
///
/// 一緒くたに「成立 N 件」と出すと、仮置きが検査済みに見える。
#[test]
fn the_stats_separate_checked_from_assumed() {
    let mut space = ProofSpace::new();
    space.add_assumption(Obligation::new("hard", "難しい命題"));
    space.add_obligation(Obligation::new("easy", "a = a"));

    let stats = space.verify_all(&TrivialVerifier);

    assert_eq!(stats.checked, 1, "検査で通ったのは 1 件");
    assert_eq!(stats.assumed, 1, "もう 1 件は仮置き");
    assert_eq!(stats.proved(), 2);
    assert_eq!(space.weakest_link(), Trust::Assumed);
}

/// 定義の本文に `axiom` や `sorry` を書いても、信頼度追跡は素通りできない。
///
/// 定義は検証器へ渡らず無条件に `Checked` として扱われる。本文が任意の
/// ソースである以上、そこが抜け道になる。構文の一致による粗い検出だが、
/// 外す方向は常に「信頼度を下げる」側である。
#[test]
fn a_definition_cannot_smuggle_in_an_axiom() {
    let mut space = ProofSpace::new();
    space.add_definition("sneaky", "axiom everything : False");
    let theorem = space.add_obligation(Obligation::new("thm", "a = a").using(&["sneaky"]));
    space.verify_all(&TrivialVerifier);

    assert_eq!(space.own_trust(theorem), Trust::Checked, "自分は検査済み");
    assert_eq!(
        space.effective_trust(theorem),
        Trust::Assumed,
        "しかし検査されていない仮定に依存している"
    );
}

#[test]
fn an_ordinary_definition_is_not_flagged() {
    let mut space = ProofSpace::new();
    space.add_definition("f", "def axiom_count : Nat := 3");
    let theorem = space.add_obligation(Obligation::new("thm", "a = a").using(&["f"]));
    space.verify_all(&TrivialVerifier);

    assert_eq!(
        space.effective_trust(theorem),
        Trust::Checked,
        "名前に axiom を含むだけでは下げない"
    );
}

// ---------------------------------------------------------------------
// 3. 信頼度の伝播
// ---------------------------------------------------------------------

#[test]
fn a_definition_never_drags_the_trust_down() {
    let mut space = ProofSpace::new();
    space.add_definition("f", "def f (t : Nat) : Nat := t");
    let theorem = space.add_obligation(Obligation::new("thm", "f t = f t").using(&["f"]));
    space.verify_all(&TrivialVerifier);

    assert_eq!(space.effective_trust(theorem), Trust::Checked);
}

/// Lean が完全に検査した定理でも、仮置きの補題に依存していれば信用できない。
#[test]
fn a_checked_theorem_is_only_as_strong_as_its_weakest_lemma() {
    let mut space = ProofSpace::new();
    space.add_definition("f", "def f (t : Nat) : Nat := t");

    // 補題は「既知」と宣言しただけ。誰も検査していない。
    let lemma = space.add_assumption(Obligation::new("f_id", "f t = t").using(&["f"]));
    // 定理そのものは完全に検査された。
    let theorem = space.add_obligation(Obligation::new("main", "f t = f t").citing(&["f_id"]));
    space.verify_all(&TrivialVerifier);
    assert_eq!(space.own_trust(lemma), Trust::Assumed);

    assert_eq!(space.own_trust(theorem), Trust::Checked, "自分は検査済み");
    assert_eq!(
        space.effective_trust(theorem),
        Trust::Assumed,
        "しかし仮置きに依存している"
    );
    assert_eq!(space.weakest_link(), Trust::Assumed);
    assert_eq!(space.below(Trust::Checked), vec!["f_id", "main"]);
}

/// 信頼度は連鎖の全長にわたって落ちる。1 段だけではない。
#[test]
fn weakness_propagates_the_whole_way_down() {
    let mut space = ProofSpace::new();
    let weak = space.add_assumption(Obligation::new("l0", "P0"));

    let mut previous = "l0".to_owned();
    let mut entities = Vec::new();
    for index in 1..5 {
        let name = format!("l{index}");
        entities.push(
            space.add_obligation(Obligation::new(&name, "Q = Q").citing(&[previous.as_str()])),
        );
        previous = name;
    }
    space.verify_all(&TrivialVerifier);
    assert_eq!(space.own_trust(weak), Trust::Assumed);

    for entity in entities {
        assert_eq!(space.own_trust(entity), Trust::Checked);
        assert_eq!(
            space.effective_trust(entity),
            Trust::Assumed,
            "4 段先まで落ちる"
        );
    }
}

/// 仮置きを本物の証明に置き換えると、下流の信頼度が上がる。
/// そのとき下流の**再検査は要らない**。命題は変わっていない。
#[test]
fn replacing_an_assumption_with_a_real_proof_lifts_everything_downstream() {
    let mut space = ProofSpace::new();
    let lemma = space.add_assumption(Obligation::new("l", "P = P"));
    let theorem = space.add_obligation(Obligation::new("t", "Q = Q").citing(&["l"]));
    space.verify_all(&TrivialVerifier);
    assert_eq!(space.effective_trust(theorem), Trust::Assumed);

    // 仮置きをやめて、本物の検証器にかける。
    assert!(space.promote_assumption(lemma));
    let verifier = CountingVerifier::new();
    space.verify_all(&verifier);

    assert_eq!(space.own_trust(lemma), Trust::Checked);
    assert_eq!(
        space.effective_trust(theorem),
        Trust::Checked,
        "下流の実効信頼度が上がる"
    );
    assert_eq!(
        verifier.calls(),
        vec!["l".to_owned(), "t".to_owned()],
        "直接の引用元は 1 回巻き込まれる（`rewriting_a_proof_stops_one_step_downstream` 参照）"
    );
}

#[test]
fn an_unverified_space_has_no_trust() {
    let space = chain();
    assert_eq!(space.weakest_link(), Trust::Unknown);
}

#[test]
fn a_space_with_only_definitions_has_no_weak_link() {
    let mut space = ProofSpace::new();
    space.add_definition("f", "def f : Nat := 1");
    assert_eq!(space.weakest_link(), Trust::Checked);
}

// ---------------------------------------------------------------------
// 文脈の組み立て
// ---------------------------------------------------------------------

/// 検証には、上流が依存順に、引用した補題は `axiom` として渡る。
#[test]
fn the_context_carries_upstream_in_dependency_order() {
    let mut space = chain();
    let verifier = AlwaysChecked::new();
    space.verify_all(&verifier);

    let seen = verifier.seen_context.borrow().clone();
    assert_eq!(seen.len(), 2, "義務は 2 件");

    // 補題の文脈: c と f の定義が、c → f の順で入る。
    assert_eq!(
        seen[0],
        vec![
            "def c : Nat := 1".to_owned(),
            "def f (t : Nat) : Nat := t + c".to_owned(),
        ]
    );

    // 定理の文脈: 引用した補題は axiom として入る。証明本体は入らない。
    assert_eq!(
        seen[1],
        vec![
            "def c : Nat := 1".to_owned(),
            "def f (t : Nat) : Nat := t + c".to_owned(),
            "axiom f_refl : f t = f t".to_owned(),
        ]
    );
}
