//! T01〜T14 — カーネル v0.1 のシナリオテスト。
//!
//! 企画書 §8 の P0 完了条件のひとつ。各テストは「利用者から見て何が起きるか」
//! を 1 つずつ固定する。番号は要件定義書のシナリオ ID に対応する。
//!
//! NOTE: `mathrel-kernel v0.1 要件定義書` はこのリポジトリに入っていない。
//! シナリオの内容は企画書 §6 とカーネル内のコメント（SPEC-GAP 注記を含む）
//! から復元したものである。要件定義書が入ったら ID の対応を照合すること。

mod support;

use mathrel_kernel::{
    Capability, EvalOutcome, Freshness, Kernel, KernelError, ValueUpdate,
};
use support::{add_function, add_value, digest_for, evaluate_all, func_bound, name_bound};

/// 鮮度を短い文字列で見る。`assert_eq!` の失敗表示を読みやすくするため。
fn freshness_of(kernel: &Kernel, entity: mathrel_kernel::Entity) -> &'static str {
    kernel.freshness(entity).expect("生存中").kind_str()
}

/// `x = 2` / `y = x + 1` の 2 段。返り値は (x, y)。
fn pair(kernel: &mut Kernel) -> (mathrel_kernel::Entity, mathrel_kernel::Entity) {
    let x = add_value(kernel, "x", vec![]);
    let requires = vec![name_bound(kernel, "x")];
    let y = add_value(kernel, "y", requires);
    (x, y)
}

/// `x` / `y = f(x)` / `z = y + 1` の 3 段。返り値は (x, y, z)。
fn chain(
    kernel: &mut Kernel,
) -> (
    mathrel_kernel::Entity,
    mathrel_kernel::Entity,
    mathrel_kernel::Entity,
) {
    let (x, y) = pair(kernel);
    let requires = vec![name_bound(kernel, "y")];
    let z = add_value(kernel, "z", requires);
    (x, y, z)
}

// ---------------------------------------------------------------------
// T01〜T02: 登録と依存の導出
// ---------------------------------------------------------------------

/// T01: 登録直後は未評価であり、評価すると `Clean` になる。
#[test]
fn t01_new_item_starts_unevaluated_then_becomes_clean() {
    let mut kernel = Kernel::new();
    let x = add_value(&mut kernel, "x", vec![]);

    assert_eq!(freshness_of(&kernel, x), "NeverEvaluated");
    assert!(kernel.resolution(x).expect("解決状態").is_resolved());
    assert!(!kernel.in_cycle(x).expect("循環判定"));
    assert_eq!(kernel.next_batch(), vec![x], "未評価なら評価対象になる");

    kernel
        .commit_evaluation(
            x,
            EvalOutcome::Value {
                digest: digest_for(x, 1),
            },
        )
        .expect("commit");

    assert_eq!(freshness_of(&kernel, x), "Clean");
    assert!(
        kernel.next_batch().is_empty(),
        "Clean なものは二度と評価されない"
    );
    assert_eq!(
        kernel.freshness(x).expect("鮮度").digest(),
        Some(digest_for(x, 1))
    );
}

/// T02: 依存の辺は `requires` / `provides` から導出される。
#[test]
fn t02_edges_are_derived_from_capabilities() {
    let mut kernel = Kernel::new();
    let (x, y) = pair(&mut kernel);

    assert_eq!(kernel.dependencies(y).expect("上流"), vec![x]);
    assert_eq!(kernel.dependents(x).expect("下流"), vec![y]);
    assert!(kernel.dependencies(x).expect("上流").is_empty());
    assert!(kernel.dependents(y).expect("下流").is_empty());

    // 評価順序は依存に従う。y は x が Clean になるまで候補にならない。
    assert_eq!(kernel.next_batch(), vec![x]);
}

// ---------------------------------------------------------------------
// T03〜T06: 変更の伝播と早期カットオフ
// ---------------------------------------------------------------------

/// T03: 上流を変えると直接下流が `Dirty` になる。
#[test]
fn t03_change_marks_direct_downstream_dirty() {
    let mut kernel = Kernel::new();
    let (x, y) = pair(&mut kernel);
    evaluate_all(&mut kernel, 1);
    assert_eq!(freshness_of(&kernel, y), "Clean");

    let report = kernel
        .change_value(x, ValueUpdate::default())
        .expect("change");

    assert_eq!(report.newly_dirty, vec![x, y]);
    assert!(report.newly_maybe_dirty.is_empty());
    assert_eq!(freshness_of(&kernel, y), "Dirty");
}

/// T04: 間接下流は `MaybeDirty` に留まる。`Dirty` にはしない。
///
/// この 2 段階が早期カットオフの前提である。間接下流まで `Dirty` にすると、
/// 上流の値が実際には変わらなかった場合の偽陽性がそのまま残る。
#[test]
fn t04_indirect_downstream_is_only_maybe_dirty() {
    let mut kernel = Kernel::new();
    let (x, y, z) = chain(&mut kernel);
    evaluate_all(&mut kernel, 1);

    let report = kernel
        .change_value(x, ValueUpdate::default())
        .expect("change");

    assert_eq!(report.newly_dirty, vec![x, y], "自分と直接下流だけが Dirty");
    assert_eq!(report.newly_maybe_dirty, vec![z], "間接下流は MaybeDirty");
    assert_eq!(freshness_of(&kernel, z), "MaybeDirty");
}

/// T05: 指紋が変わらなければ、下流は再計算されずに `Clean` へ戻る。
#[test]
fn t05_early_cutoff_skips_downstream_when_digest_is_unchanged() {
    let mut kernel = Kernel::new();
    let (x, y, z) = chain(&mut kernel);
    evaluate_all(&mut kernel, 1);
    kernel
        .change_value(x, ValueUpdate::default())
        .expect("change");

    // 同じ seed で評価する = どのエンティティも前回と同じ指紋を返す。
    let order = kernel.run_to_fixpoint(|_, entity| EvalOutcome::Value {
        digest: digest_for(entity, 1),
    });

    assert_eq!(order, vec![x, y], "z は一度も評価されない");
    assert_eq!(freshness_of(&kernel, z), "Clean", "z は Clean に戻る");
    assert!(kernel.check_invariants().is_empty());
}

/// T06: 指紋が変われば、`MaybeDirty` は `Dirty` に昇格して再計算される。
#[test]
fn t06_changed_digest_promotes_maybe_dirty_downstream() {
    let mut kernel = Kernel::new();
    let (x, y, z) = chain(&mut kernel);
    evaluate_all(&mut kernel, 1);
    kernel
        .change_value(x, ValueUpdate::default())
        .expect("change");

    // seed を変える = すべてのエンティティの指紋が変わる。
    let order = kernel.run_to_fixpoint(|_, entity| EvalOutcome::Value {
        digest: digest_for(entity, 2),
    });

    assert_eq!(order, vec![x, y, z], "連鎖の末端まで再計算される");
    assert_eq!(freshness_of(&kernel, z), "Clean");
    assert!(kernel.check_invariants().is_empty());
}

// ---------------------------------------------------------------------
// T07〜T10: 参照解決
// ---------------------------------------------------------------------

/// T07: provider のない `requires` は、エラーではなく `Unresolved` という状態になる。
#[test]
fn t07_missing_provider_is_a_state_not_an_error() {
    let mut kernel = Kernel::new();
    let missing = name_bound(&mut kernel, "x");
    let y = add_value(&mut kernel, "y", vec![missing]);

    let resolution = kernel.resolution(y).expect("解決状態");
    assert!(resolution.is_unresolved());
    assert_eq!(resolution.missing(), &[missing]);
    assert_eq!(kernel.unresolved(), vec![(y, vec![missing])]);

    assert!(
        kernel.next_batch().is_empty(),
        "未解決のものは評価対象にしない"
    );
    assert_eq!(
        kernel.commit_evaluation(y, EvalOutcome::Value { digest: [0; 32] }),
        Err(KernelError::CommitOnUnresolved(y)),
        "未解決のまま評価結果を受け取らない"
    );
}

/// T08: 後から provider を足すと解決し、評価できるようになる。
#[test]
fn t08_adding_a_provider_resolves_the_dangling_reference() {
    let mut kernel = Kernel::new();
    let missing = name_bound(&mut kernel, "x");
    let y = add_value(&mut kernel, "y", vec![missing]);

    let x = add_value(&mut kernel, "x", vec![]);

    assert!(kernel.resolution(y).expect("解決状態").is_resolved());
    assert_eq!(kernel.dependencies(y).expect("上流"), vec![x]);
    assert!(kernel.unresolved().is_empty());
    assert_eq!(kernel.next_batch(), vec![x], "まず x が評価対象になる");

    evaluate_all(&mut kernel, 1);
    assert_eq!(freshness_of(&kernel, y), "Clean");
}

/// T08b: 解決したことは `ChangeReport` にも現れる。
#[test]
fn t08_resolution_appears_in_the_change_report() {
    let mut kernel = Kernel::new();
    let cap = name_bound(&mut kernel, "x");
    let y = add_value(&mut kernel, "y", vec![cap]);

    let (_, report) = kernel
        .add_item(mathrel_kernel::ItemSpec {
            provides: vec![cap],
            ..Default::default()
        })
        .expect("add x");

    assert_eq!(report.newly_resolved, vec![y]);
}

/// T09: provider が複数あるときは `Ambiguous` になるが、評価は止めない。
///
/// 曖昧なときは全 provider へ辺を張る。どちらが変わっても dirty が伝わる
/// 側（健全側）へ倒すためである。
#[test]
fn t09_multiple_providers_are_ambiguous_but_still_evaluable() {
    let mut kernel = Kernel::new();
    let x1 = add_value(&mut kernel, "x", vec![]);
    let x2 = add_value(&mut kernel, "x", vec![]);
    let cap = name_bound(&mut kernel, "x");
    let y = add_value(&mut kernel, "y", vec![cap]);

    let resolution = kernel.resolution(y).expect("解決状態");
    assert!(resolution.is_ambiguous());
    assert!(!resolution.is_unresolved());
    assert_eq!(resolution.conflicts(), &[(cap, vec![x1, x2])]);
    assert_eq!(
        kernel.dependencies(y).expect("上流"),
        vec![x1, x2],
        "全 provider へ辺を張る"
    );

    evaluate_all(&mut kernel, 1);
    assert_eq!(freshness_of(&kernel, y), "Clean", "曖昧でも評価は進む");

    // どちらの provider を変えても y に伝わる。
    let report = kernel
        .change_value(x2, ValueUpdate::default())
        .expect("change");
    assert!(report.newly_dirty.contains(&y));
}

/// T10: `f = 3` と `f(t) = t^2` は別の Capability であり、衝突しない。
#[test]
fn t10_value_and_function_bindings_do_not_collide() {
    let mut kernel = Kernel::new();
    let f_value = add_value(&mut kernel, "f", vec![]);
    let f_function = add_function(&mut kernel, "f", 1, vec![]);

    let call = func_bound(&mut kernel, "f", 1);
    let caller = add_value(&mut kernel, "y", vec![call]);
    assert!(kernel.resolution(caller).expect("解決状態").is_resolved());
    assert_eq!(
        kernel.dependencies(caller).expect("上流"),
        vec![f_function],
        "関数呼び出しは関数定義だけに依存する"
    );

    let read = name_bound(&mut kernel, "f");
    let reader = add_value(&mut kernel, "z", vec![read]);
    assert_eq!(
        kernel.dependencies(reader).expect("上流"),
        vec![f_value],
        "名前参照は値定義だけに依存する"
    );

    // アリティが違えば別物である。
    let wrong_arity = func_bound(&mut kernel, "f", 2);
    let bad = add_value(&mut kernel, "w", vec![wrong_arity]);
    assert!(kernel.resolution(bad).expect("解決状態").is_unresolved());
}

// ---------------------------------------------------------------------
// T11〜T12: 循環
// ---------------------------------------------------------------------

/// T11: 循環は検出され、評価から外れる。エラーにはしない。
#[test]
fn t11_cycles_are_detected_and_excluded_from_evaluation() {
    let mut kernel = Kernel::new();
    let cap_a = name_bound(&mut kernel, "a");
    let cap_b = name_bound(&mut kernel, "b");

    let a = add_value(&mut kernel, "a", vec![cap_b]);
    let (b, report) = kernel
        .add_item(mathrel_kernel::ItemSpec {
            provides: vec![cap_b],
            requires: vec![cap_a],
            ..Default::default()
        })
        .expect("add b");

    assert_eq!(report.cycles_formed, vec![vec![a, b]]);
    assert!(kernel.in_cycle(a).expect("循環判定"));
    assert!(kernel.in_cycle(b).expect("循環判定"));
    assert_eq!(kernel.cycles(), vec![vec![a, b]]);

    assert!(kernel.next_batch().is_empty(), "循環は評価対象にしない");
    assert_eq!(
        kernel.commit_evaluation(a, EvalOutcome::Value { digest: [0; 32] }),
        Err(KernelError::CommitOnCyclic(a))
    );
}

/// T12: 循環を切ると、両方が評価できるようになる。
#[test]
fn t12_breaking_a_cycle_restores_evaluation() {
    let mut kernel = Kernel::new();
    let cap_a = name_bound(&mut kernel, "a");
    let cap_b = name_bound(&mut kernel, "b");
    let a = add_value(&mut kernel, "a", vec![cap_b]);
    let (b, _) = kernel
        .add_item(mathrel_kernel::ItemSpec {
            provides: vec![cap_b],
            requires: vec![cap_a],
            ..Default::default()
        })
        .expect("add b");

    // b が a を参照するのをやめる。
    let report = kernel
        .change_value(
            b,
            ValueUpdate {
                requires: Some(vec![]),
                ..Default::default()
            },
        )
        .expect("change");

    assert_eq!(report.cycles_broken, vec![vec![a, b]]);
    assert!(kernel.cycles().is_empty());
    assert!(!kernel.in_cycle(a).expect("循環判定"));
    assert!(!kernel.in_cycle(b).expect("循環判定"));

    let order = evaluate_all(&mut kernel, 1);
    assert_eq!(order, vec![b, a], "b が先、それに依存する a が後");
    assert!(kernel.check_invariants().is_empty());
}

// ---------------------------------------------------------------------
// T13〜T14: 削除と失敗
// ---------------------------------------------------------------------

/// T13: 削除しても、依存していたものは消えない。未解決になって残る。
///
/// 「未完成のまま保持する」ことがワークスペースの前提だからである。
#[test]
fn t13_removal_leaves_dependents_unresolved_not_deleted() {
    let mut kernel = Kernel::new();
    let (x, y) = pair(&mut kernel);
    evaluate_all(&mut kernel, 1);

    let report = kernel.remove_item(x).expect("remove");

    assert_eq!(report.newly_unresolved, vec![y]);
    assert_eq!(kernel.entities(), vec![y], "y は残る");
    assert!(kernel.resolution(y).expect("解決状態").is_unresolved());
    assert_eq!(freshness_of(&kernel, y), "Dirty");
    assert!(kernel.dependencies(y).expect("上流").is_empty());

    // 削除済みハンドルはパニックせずエラーになる。
    assert_eq!(
        kernel.freshness(x),
        Err(KernelError::StaleEntity(x)),
        "世代が上がるので古いハンドルは検出できる"
    );
}

/// T14: 評価に失敗したものは、入力が変わるまで再試行しない。変えれば再試行する。
///
/// 回帰テスト: 失敗の記録を消し損ねると、式を直しても二度と再計算されない。
#[test]
fn t14_failed_items_retry_only_after_something_changes() {
    let mut kernel = Kernel::new();
    let x = add_value(&mut kernel, "x", vec![]);

    kernel
        .commit_evaluation(
            x,
            EvalOutcome::Failed {
                reason: "0 で割れない".to_owned(),
            },
        )
        .expect("commit");

    assert_eq!(freshness_of(&kernel, x), "Dirty");
    assert_eq!(kernel.failure(x).expect("失敗理由"), Some("0 で割れない"));
    assert!(
        kernel.next_batch().is_empty(),
        "同じ入力で再試行しても同じ結果にしかならない"
    );

    kernel
        .change_value(
            x,
            ValueUpdate {
                source: Some("x = 2".to_owned()),
                ..Default::default()
            },
        )
        .expect("change");

    assert_eq!(kernel.failure(x).expect("失敗理由"), None, "失敗は消える");
    assert_eq!(kernel.next_batch(), vec![x], "直せば再計算される");

    kernel
        .commit_evaluation(
            x,
            EvalOutcome::Value {
                digest: digest_for(x, 1),
            },
        )
        .expect("commit");
    assert_eq!(freshness_of(&kernel, x), "Clean");
}

/// T14b: 上流が変わったときも、失敗した下流は再試行される。
#[test]
fn t14_upstream_change_retries_a_failed_downstream() {
    let mut kernel = Kernel::new();
    let (x, y) = pair(&mut kernel);
    evaluate_all(&mut kernel, 1);

    // y の評価だけが失敗している状態を作る。
    kernel
        .change_value(y, ValueUpdate::default())
        .expect("change");
    kernel
        .commit_evaluation(
            y,
            EvalOutcome::Failed {
                reason: "型が合わない".to_owned(),
            },
        )
        .expect("commit");
    assert!(kernel.next_batch().is_empty());

    // x を変えれば y の入力が変わる。もう一度試すべきである。
    kernel
        .change_value(x, ValueUpdate::default())
        .expect("change");
    assert_eq!(kernel.next_batch(), vec![x]);
    evaluate_all(&mut kernel, 2);
    assert_eq!(freshness_of(&kernel, y), "Clean");
    assert_eq!(kernel.failure(y).expect("失敗理由"), None);
}

// ---------------------------------------------------------------------
// T15: 検証義務も同じグラフに載る
// ---------------------------------------------------------------------

/// T15: 定理は定義に依存し、定義が変われば「要再検査」になる。
///
/// 検証層（`mathrel-verify`）の前提。証明の追跡に専用の機構は要らず、
/// `Capability::PropositionProved` を 1 つ足せば既存のグラフに載る。
/// 詳細は `docs/検証層の設計.md` ADR-006。
#[test]
fn t15_proof_obligations_live_in_the_same_graph_as_values() {
    let mut kernel = Kernel::new();

    // c = 1
    let c = add_value(&mut kernel, "c", vec![]);
    // f(t) = t^2 + c
    let needs_c = vec![name_bound(&mut kernel, "c")];
    let f = add_function(&mut kernel, "f", 1, needs_c);

    // 補題: f は非負である（f に依存する）
    let lemma_name = kernel.intern("f_nonneg");
    let calls_f = vec![func_bound(&mut kernel, "f", 1)];
    let lemma = kernel
        .add_item(mathrel_kernel::ItemSpec {
            provides: vec![Capability::PropositionProved(lemma_name)],
            requires: calls_f,
            source: Some("lemma f_nonneg : ∀ t, 0 ≤ f t".to_owned()),
            ..Default::default()
        })
        .expect("add lemma")
        .0;

    // 定理: 補題を引用する
    let theorem_name = kernel.intern("f_min");
    let theorem = kernel
        .add_item(mathrel_kernel::ItemSpec {
            provides: vec![Capability::PropositionProved(theorem_name)],
            requires: vec![Capability::PropositionProved(lemma_name)],
            source: Some("theorem f_min : ...".to_owned()),
            ..Default::default()
        })
        .expect("add theorem")
        .0;

    assert_eq!(kernel.dependencies(lemma).expect("上流"), vec![f]);
    assert_eq!(kernel.dependencies(theorem).expect("上流"), vec![lemma]);

    evaluate_all(&mut kernel, 1);
    assert_eq!(freshness_of(&kernel, theorem), "Clean");

    // c を変えると、値の連鎖と証明の連鎖の両方に伝播する。
    let report = kernel
        .change_value(c, ValueUpdate::default())
        .expect("change");
    assert!(report.newly_dirty.contains(&f), "定義が古くなる");
    assert!(report.newly_maybe_dirty.contains(&lemma), "補題も古くなる");
    assert!(report.newly_maybe_dirty.contains(&theorem), "定理も古くなる");

    // 証明の再検査は高価なので、早期カットオフが効くことが本質である。
    // c の指紋が変わらなければ、補題も定理も再検査されない。
    let order = kernel.run_to_fixpoint(|_, entity| EvalOutcome::Value {
        digest: digest_for(entity, 1),
    });
    assert_eq!(order, vec![c, f], "証明は 1 つも再検査されない");
    assert_eq!(freshness_of(&kernel, theorem), "Clean");
}

/// 値の束縛と命題は、同じ名前でも別物である。
#[test]
fn t15_a_name_can_be_both_a_value_and_a_proposition() {
    let mut kernel = Kernel::new();
    let name = kernel.intern("P");

    let value = add_value(&mut kernel, "P", vec![]);
    let proposition = kernel
        .add_item(mathrel_kernel::ItemSpec {
            provides: vec![Capability::PropositionProved(name)],
            ..Default::default()
        })
        .expect("add proposition")
        .0;

    let reads_value = add_value(&mut kernel, "y", vec![Capability::NameBound(name)]);
    let cites_proof = kernel
        .add_item(mathrel_kernel::ItemSpec {
            requires: vec![Capability::PropositionProved(name)],
            ..Default::default()
        })
        .expect("add citation")
        .0;

    assert_eq!(kernel.dependencies(reads_value).expect("上流"), vec![value]);
    assert_eq!(
        kernel.dependencies(cites_proof).expect("上流"),
        vec![proposition]
    );
    assert_eq!(
        kernel.capability_label(Capability::PropositionProved(name)),
        "PropositionProved(P)"
    );
}

// ---------------------------------------------------------------------
// 横断: どのシナリオでも壊れてはいけないもの
// ---------------------------------------------------------------------

/// `Clean` なものの上流は必ず `Clean` である。この不変条件が破れると、
/// 「古い入力で計算された結果」が Clean を名乗ることになる。
#[test]
fn clean_items_never_have_stale_upstream() {
    let mut kernel = Kernel::new();
    let (x, y, z) = chain(&mut kernel);
    evaluate_all(&mut kernel, 1);
    assert!(kernel.check_invariants().is_empty());

    kernel
        .change_value(x, ValueUpdate::default())
        .expect("change");
    assert!(
        kernel.check_invariants().is_empty(),
        "伝播の途中でも破れない"
    );

    // x だけを評価した中間状態でも破れない。
    kernel
        .commit_evaluation(
            x,
            EvalOutcome::Value {
                digest: digest_for(x, 9),
            },
        )
        .expect("commit");
    assert!(kernel.check_invariants().is_empty());
    assert_eq!(freshness_of(&kernel, y), "Dirty");
    assert_ne!(freshness_of(&kernel, z), "Clean");

    evaluate_all(&mut kernel, 9);
    assert!(kernel.check_invariants().is_empty());
    for entity in [x, y, z] {
        assert!(matches!(
            kernel.freshness(entity).expect("鮮度"),
            Freshness::Clean { .. }
        ));
    }
}

/// 曖昧なまま provider が増えたときも、下流は古くなる。
///
/// 回帰テスト。解決状態の**種別**は `Ambiguous` のまま変わらないが、上流の
/// 集合には新しい辺が増えている。種別の変化だけを見ていると、新たな上流を
/// 獲得した（この例では循環に入った）エンティティが `Clean` を名乗り続ける。
/// 性質テスト P2 が見つけた。
#[test]
fn gaining_an_upstream_while_staying_ambiguous_still_dirties() {
    let mut kernel = Kernel::new();
    let cap_a = name_bound(&mut kernel, "a");
    let cap_b = name_bound(&mut kernel, "b");

    // a を提供するものが 2 つある。b はその両方に依存する（Ambiguous）。
    add_value(&mut kernel, "a", vec![]);
    add_value(&mut kernel, "a", vec![]);
    let b = add_value(&mut kernel, "b", vec![cap_a]);
    evaluate_all(&mut kernel, 1);
    assert!(kernel.resolution(b).expect("解決状態").is_ambiguous());
    assert_eq!(freshness_of(&kernel, b), "Clean");

    // 3 つ目の a を足す。ただしそれは b に依存している。
    let third = kernel
        .add_item(mathrel_kernel::ItemSpec {
            provides: vec![cap_a],
            requires: vec![cap_b],
            ..Default::default()
        })
        .expect("add third")
        .0;

    // 種別は Ambiguous のまま。しかし b の上流は 2 件から 3 件に増えている。
    assert!(kernel.resolution(b).expect("解決状態").is_ambiguous());
    assert_eq!(kernel.dependencies(b).expect("上流").len(), 3);

    assert_ne!(
        freshness_of(&kernel, b),
        "Clean",
        "新しい上流を得た以上、Clean のままではいられない"
    );
    assert!(kernel.in_cycle(b).expect("循環判定"), "b と third は循環する");
    assert!(kernel.in_cycle(third).expect("循環判定"));
    assert!(kernel.check_invariants().is_empty());
}

/// 存在しないハンドルはパニックではなくエラーになる。
#[test]
fn unknown_handles_are_errors_not_panics() {
    let mut kernel = Kernel::new();
    let x = add_value(&mut kernel, "x", vec![]);
    kernel.remove_item(x).expect("remove");

    assert_eq!(kernel.dependencies(x), Err(KernelError::StaleEntity(x)));
    assert_eq!(kernel.remove_item(x), Err(KernelError::StaleEntity(x)));
    assert_eq!(
        kernel.change_value(x, ValueUpdate::default()),
        Err(KernelError::StaleEntity(x))
    );
}

/// `Capability` の表示は人間が読める。UI とスナップショットで使う。
#[test]
fn capability_labels_are_readable() {
    let mut kernel = Kernel::new();
    let value = name_bound(&mut kernel, "x");
    let function = func_bound(&mut kernel, "f", 2);
    let type_known = support::type_known(&mut kernel, "T");

    assert_eq!(kernel.capability_label(value), "NameBound(x)");
    assert_eq!(kernel.capability_label(function), "FunctionBound(f/2)");
    assert_eq!(kernel.capability_label(type_known), "TypeKnown(T)");
    assert_eq!(Capability::NameBound(kernel.intern("x")).kind_str(), "NameBound");
}
