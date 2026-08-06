//! P1〜P6 — カーネル v0.1 の性質テスト。
//!
//! 企画書 §8 の P0 完了条件「P1〜P6 の性質テストが 1000 ケース以上で green」に
//! 対応する。素朴実装（オラクル）は `support::OracleKernel` にある。
//!
//! NOTE: `mathrel-kernel v0.1 要件定義書` はこのリポジトリに入っていない。
//! 各性質の内容は、カーネル内の言及から復元した。
//!
//! | ID | 性質 | 出典 |
//! |----|------|------|
//! | P1 | 素朴実装との等価性（早期カットオフ無効時） | `kernel.rs` `set_early_cutoff` / `stale_set` |
//! | P2 | 評価可能なものは必ず `Clean` に到達する | `kernel.rs` `next_batch` の SPEC-GAP 注記 |
//! | P3 | 評価は必ず収束する | 同上 |
//! | P4 | `Clean` の上流は必ず `Clean`（不変条件） | `kernel.rs` `check_invariants` |
//! | P5 | 決定性（同じ操作列は同じ状態を生む） | `graph.rs` 冒頭 |
//! | P6 | 早期カットオフの健全性と精度 | `state.rs` `Freshness` / 企画書 §8 |
//!
//! ## 生成器を 2 種類持つ理由
//!
//! ランダムな `provides` / `requires` の組は、循環・未解決・曖昧をよく踏む
//! 代わりに、**深い依存の連鎖をほとんど作らない**。実測で、間接下流が存在する
//! ケースは 1000 中 5 件しかなかった。早期カットオフの効き目は連鎖がないと
//! 測れないので、層状 DAG を作る生成器を別に用意している。
//! 空振りしていないことは `generators_cover_interesting_states` で監視する。

mod support;

use mathrel_kernel::Freshness;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use std::cell::Cell;
use std::collections::BTreeSet;
use support::{Model, OracleItem};

/// ランダム生成で使う Capability の種類数。少ないほど衝突と循環が出る。
const RANDOM_CAPABILITIES: u16 = 4;
/// DAG 生成で使う Capability の種類数。項目数以上でなければならない。
const DAG_CAPABILITIES: u16 = 8;
/// 操作対象として生成する添字の上限。範囲外は無視されるので安全である。
const INDEX_RANGE: usize = 8;

// ---------------------------------------------------------------------
// 生成
// ---------------------------------------------------------------------

fn item_strategy() -> impl Strategy<Value = OracleItem> {
    (
        prop::collection::vec(0u16..RANDOM_CAPABILITIES, 0..=2),
        prop::collection::vec(0u16..RANDOM_CAPABILITIES, 0..=2),
    )
        .prop_map(|(provides, requires)| OracleItem { provides, requires })
}

/// 何でもありの構成。循環・未解決・曖昧を踏ませるための生成器。
fn random_strategy() -> impl Strategy<Value = Vec<OracleItem>> {
    prop::collection::vec(item_strategy(), 1..7)
}

/// 層状 DAG。項目 `i` は capability `i` を提供し、`i` 未満だけを要求する。
///
/// 構成上、循環せず未解決も出ない。伝播の深さを確実に作るための生成器である。
fn dag_strategy() -> impl Strategy<Value = Vec<OracleItem>> {
    (2usize..=6).prop_flat_map(|count| {
        let layers: Vec<_> = (0..count)
            .map(|index| {
                let upper = u16::try_from(index.max(1)).expect("小さい");
                let size = if index == 0 { 0..=0 } else { 1..=2 };
                prop::collection::vec(0u16..upper, size)
            })
            .collect();
        layers.prop_map(|rows| {
            rows.into_iter()
                .enumerate()
                .map(|(index, mut requires)| {
                    requires.sort_unstable();
                    requires.dedup();
                    OracleItem {
                        provides: vec![u16::try_from(index).expect("小さい")],
                        requires,
                    }
                })
                .collect()
        })
    })
}

/// 2 つの生成器を混ぜる。Capability の種類数は形ごとに変える。
fn workspace_strategy() -> impl Strategy<Value = (u16, Vec<OracleItem>)> {
    prop_oneof![
        random_strategy().prop_map(|items| (RANDOM_CAPABILITIES, items)),
        dag_strategy().prop_map(|items| (DAG_CAPABILITIES, items)),
    ]
}

/// カーネルへの操作。オラクルにも同じものを適用する。
#[derive(Clone, Debug)]
enum Op {
    Add(OracleItem),
    Change(usize, OracleItem),
    Touch(usize),
    Remove(usize),
    Evaluate(u8),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => item_strategy().prop_map(Op::Add),
        3 => (0..INDEX_RANGE, item_strategy()).prop_map(|(index, item)| Op::Change(index, item)),
        2 => (0..INDEX_RANGE).prop_map(Op::Touch),
        1 => (0..INDEX_RANGE).prop_map(Op::Remove),
        3 => (1u8..4).prop_map(Op::Evaluate),
    ]
}

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(op_strategy(), 0..12)
}

fn apply(model: &mut Model, op: &Op) {
    match op {
        Op::Add(item) => {
            model.add(item.clone());
        }
        Op::Change(index, item) => {
            let _ = model.change(*index, item.clone());
        }
        Op::Touch(index) => {
            let _ = model.touch(*index);
        }
        Op::Remove(index) => {
            let _ = model.remove(*index);
        }
        Op::Evaluate(seed) => {
            model.evaluate(*seed);
        }
    }
}

fn build(capabilities: u16, items: Vec<OracleItem>) -> Model {
    let mut model = Model::new(capabilities);
    for item in items {
        model.add(item);
    }
    model
}

/// `clean` の中から 1 つ選ぶ。決定的に選ぶので縮小（shrink）が効く。
fn pick(clean: &BTreeSet<usize>, seed: usize) -> Option<usize> {
    if clean.is_empty() {
        return None;
    }
    clean.iter().nth(seed % clean.len()).copied()
}

/// 状態の観測。決定性テスト（P5）で 2 つの実行を突き合わせる。
fn observe(model: &Model) -> Vec<String> {
    let mut lines = Vec::new();
    for index in 0..model.entities.len() {
        let entity = match model.entity(index) {
            Some(entity) => entity,
            None => {
                lines.push(format!("{index}: removed"));
                continue;
            }
        };
        let freshness = model
            .kernel
            .freshness(entity)
            .map(Freshness::kind_str)
            .unwrap_or("?");
        let resolution = model
            .kernel
            .resolution(entity)
            .map(|resolution| resolution.kind_str())
            .unwrap_or("?");
        let in_cycle = model.kernel.in_cycle(entity).unwrap_or(false);
        let mut upstream: Vec<String> = model
            .kernel
            .dependencies(entity)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|provider| model.index_of(provider))
            .map(|index| index.to_string())
            .collect();
        upstream.sort();
        lines.push(format!(
            "{index}: {freshness} {resolution} cycle={in_cycle} upstream=[{}]",
            upstream.join(",")
        ));
    }
    lines.push(format!("stale={:?}", model.stale_indices()));
    lines
}

// ---------------------------------------------------------------------
// 性質
// ---------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// P1: 早期カットオフを切れば、古くなる範囲は素朴実装と完全に一致する。
    ///
    /// 素朴実装は「変更点から到達できるものは全部古い」と答える。カーネルは
    /// 増分保守で同じ答えを出さなければならない。比較対象を変更前に `Clean`
    /// だったものに絞るのは、未評価のもの（循環・未解決とその下流）は
    /// そもそも「古くなる」対象ではないからである。
    #[test]
    fn p1_stale_set_matches_the_naive_oracle(
        (capabilities, items) in workspace_strategy(),
        seed in 0usize..64,
    ) {
        let mut model = Model::new(capabilities);
        model.set_early_cutoff(false);
        for item in items {
            model.add(item);
        }
        model.evaluate(1);

        let clean_before = model.clean_indices();
        let target = match pick(&clean_before, seed) {
            Some(target) => target,
            None => return Ok(()),
        };

        model.touch(target).expect("touch");

        let expected: BTreeSet<usize> = model
            .oracle
            .stale_after_change(target)
            .into_iter()
            .filter(|index| clean_before.contains(index))
            .collect();
        let actual: BTreeSet<usize> = model
            .stale_indices()
            .into_iter()
            .filter(|index| *index != target && clean_before.contains(index))
            .collect();

        prop_assert_eq!(actual, expected);
    }

    /// P2: 評価できるものは必ず `Clean` に到達し、できないものは決して `Clean` にならない。
    ///
    /// 「評価できる」とは、自分と全上流が、循環に含まれず未解決でもないこと。
    /// 循環や未解決を含む部分は未評価のまま残るが、それは正常な状態である。
    #[test]
    fn p2_everything_evaluable_reaches_clean(
        (capabilities, items) in workspace_strategy(),
        ops in ops_strategy(),
    ) {
        let mut model = build(capabilities, items);
        for op in &ops {
            apply(&mut model, op);
        }
        model.evaluate(7);

        for index in model.oracle.alive() {
            let entity = match model.entity(index) {
                Some(entity) => entity,
                None => continue,
            };
            let is_clean = matches!(
                model.kernel.freshness(entity).expect("生存中"),
                Freshness::Clean { .. }
            );
            prop_assert_eq!(
                is_clean,
                model.oracle.is_evaluable(index),
                "index {} の到達性が素朴判定と食い違う", index
            );
        }
    }

    /// P3: 評価は必ず収束する。止まった時点で、やり残しがあってはならない。
    ///
    /// `next_batch` が空でないのに評価が進まない状態（ライブロック）に陥ると、
    /// ホストの評価ループが空回りする。
    #[test]
    fn p3_evaluation_converges(
        (capabilities, items) in workspace_strategy(),
        ops in ops_strategy(),
    ) {
        let mut model = build(capabilities, items);
        for op in &ops {
            apply(&mut model, op);
        }
        model.evaluate(3);
        prop_assert!(
            model.kernel.next_batch().is_empty(),
            "収束後に評価待ちが残っている: {:?}", model.kernel.next_batch()
        );
    }

    /// P4: `Clean` なものの上流は必ず `Clean` である。
    ///
    /// これが破れると「古い入力で計算した結果」が `Clean` を名乗ることになり、
    /// 鮮度追跡が意味を失う。操作列のどの時点でも破れてはならない。
    #[test]
    fn p4_clean_items_never_depend_on_stale_ones(
        (capabilities, items) in workspace_strategy(),
        ops in ops_strategy(),
    ) {
        let mut model = build(capabilities, items);
        prop_assert!(model.kernel.check_invariants().is_empty());
        for op in &ops {
            apply(&mut model, op);
            let violations = model.kernel.check_invariants();
            prop_assert!(
                violations.is_empty(),
                "{:?} の直後に不変条件が破れた: {:?}", op, violations
            );
        }
    }

    /// P5: 同じ操作列は必ず同じ状態を生む。
    ///
    /// 反復順序が非決定的な集合を使うと、`ChangeReport` の内容が実行ごとに
    /// 変わる。そうなると差分表示も、このテスト自体も意味を失う。
    #[test]
    fn p5_operations_are_deterministic(
        (capabilities, items) in workspace_strategy(),
        ops in ops_strategy(),
    ) {
        let mut first = build(capabilities, items.clone());
        let mut second = build(capabilities, items);
        for op in &ops {
            apply(&mut first, op);
            apply(&mut second, op);
        }
        prop_assert_eq!(observe(&first), observe(&second));
        prop_assert_eq!(first.kernel.cycles(), second.kernel.cycles());
        prop_assert_eq!(first.kernel.revision(), second.kernel.revision());
    }

    /// P6（健全性）: 早期カットオフを入れても、実際に値が変わったものは取りこぼさない。
    ///
    /// カットオフは「値が変わらなかったとき」にだけ効く最適化である。値が変われば
    /// 到達可能な下流はすべて再計算されなければならない。
    #[test]
    fn p6_early_cutoff_never_misses_a_real_change(
        (capabilities, items) in workspace_strategy(),
        seed in 0usize..64,
    ) {
        let mut model = build(capabilities, items);
        model.evaluate(1);

        let clean_before = model.clean_indices();
        let target = match pick(&clean_before, seed) {
            Some(target) => target,
            None => return Ok(()),
        };
        model.touch(target).expect("touch");

        // seed を変える = すべてのエンティティの指紋が変わる。
        let reevaluated: BTreeSet<usize> = model.evaluate_indices(2).into_iter().collect();
        let must_reevaluate: BTreeSet<usize> = model
            .oracle
            .stale_after_change(target)
            .into_iter()
            .filter(|index| clean_before.contains(index))
            .collect();

        for index in &must_reevaluate {
            prop_assert!(
                reevaluated.contains(index),
                "index {} が再計算されなかった（再計算されたのは {:?}）", index, reevaluated
            );
        }
    }

    /// P6（精度）: 値が変わらなければ、再計算は変更点と直接下流だけで止まる。
    ///
    /// 間接下流が 1 つでも再計算されたら、それは偽陽性である。企画書 §8 の
    /// 中止条件（深さ 5 の連鎖で偽陽性率 50% 超）は、この性質が成り立つ限り
    /// 発生し得ない。
    #[test]
    fn p6_early_cutoff_stops_at_direct_downstream(
        (capabilities, items) in workspace_strategy(),
        seed in 0usize..64,
    ) {
        let mut model = build(capabilities, items);
        model.evaluate(1);

        let clean_before = model.clean_indices();
        let target = match pick(&clean_before, seed) {
            Some(target) => target,
            None => return Ok(()),
        };
        model.touch(target).expect("touch");

        // 同じ seed = どのエンティティも前回と同じ指紋を返す。
        let reevaluated: BTreeSet<usize> = model.evaluate_indices(1).into_iter().collect();

        let mut expected: BTreeSet<usize> = model
            .oracle
            .downstream(target)
            .into_iter()
            .filter(|index| clean_before.contains(index))
            .collect();
        expected.insert(target);

        prop_assert_eq!(
            reevaluated,
            expected,
            "カットオフが直接下流より先へ広がった"
        );
    }
}

// ---------------------------------------------------------------------
// 深さ 5 の連鎖（企画書 §8 の中止条件に直接対応する決定的なテスト）
// ---------------------------------------------------------------------

/// 長さ `length` の直鎖 `0 → 1 → ... → length-1` を作る。
fn straight_chain(length: usize) -> Model {
    let mut model = Model::new(DAG_CAPABILITIES);
    for index in 0..length {
        let requires = if index == 0 {
            vec![]
        } else {
            vec![u16::try_from(index - 1).expect("小さい")]
        };
        model.add(OracleItem {
            provides: vec![u16::try_from(index).expect("小さい")],
            requires,
        });
    }
    model
}

/// 深さ 5 の連鎖で、値が変わらなければ偽陽性は 0 件である。
///
/// 企画書 §8 の中止条件は「早期カットオフを入れても、深さ 5 の連鎖で偽陽性率が
/// 50% を超える場合、依存追跡の粒度設計を根本から見直す」であった。実測値を
/// ここで固定する。
#[test]
fn a_depth_five_chain_has_no_false_positives() {
    let mut model = straight_chain(6);
    model.evaluate(1);
    assert_eq!(model.clean_indices().len(), 6, "6 段すべてが Clean になる");

    model.touch(0).expect("touch");
    assert_eq!(
        model.stale_indices(),
        (0..6).collect(),
        "変更直後は連鎖全体が古い候補になる"
    );

    // 先頭を評価しても値が変わらなかった場合。
    let reevaluated = model.evaluate_indices(1);
    assert_eq!(
        reevaluated,
        vec![0, 1],
        "再計算は変更点と直接下流の 2 件で止まる（偽陽性 0 件 / 4 件中）"
    );
    assert_eq!(model.clean_indices().len(), 6, "連鎖全体が Clean に戻る");
    assert!(model.kernel.check_invariants().is_empty());
}

/// 深さ 5 の連鎖で、値が変われば末端まで届く。
#[test]
fn a_depth_five_chain_propagates_a_real_change() {
    let mut model = straight_chain(6);
    model.evaluate(1);
    model.touch(0).expect("touch");

    let reevaluated = model.evaluate_indices(2);
    assert_eq!(reevaluated, vec![0, 1, 2, 3, 4, 5], "末端まで再計算される");
    assert_eq!(model.clean_indices().len(), 6);
}

/// 連鎖の途中で値が変わらなくなったら、そこで止まる。
///
/// 早期カットオフの本題である。先頭は変わったが 2 段目の結果が変わらなければ、
/// 3 段目以降は再計算する必要がない。
#[test]
fn propagation_stops_where_the_digest_stops_changing() {
    let mut model = straight_chain(6);
    model.evaluate(1);
    model.touch(0).expect("touch");

    // 添字 0 だけ指紋が変わり、それ以外は前回と同じ値を返す評価器。
    let changed = model.entity(0).expect("先頭");
    let order: Vec<usize> = model
        .kernel
        .run_to_fixpoint(|_, entity| mathrel_kernel::EvalOutcome::Value {
            digest: support::digest_for(entity, if entity == changed { 2 } else { 1 }),
        })
        .into_iter()
        .filter_map(|entity| model.index_of(entity))
        .collect();

    assert_eq!(
        order,
        vec![0, 1],
        "1 段目の値が変わらなければ 2 段目より先へは進まない"
    );
    assert_eq!(model.clean_indices().len(), 6);
    assert!(model.kernel.check_invariants().is_empty());
}

/// 合流点は、長い方の枝が確定してから `Clean` に戻る。
///
/// 回帰テスト。`0 → 1 → 2 → 3 → 4` に `1 → 4` を足した形で、4 は 1 の直接下流
/// でありながら 3 経由の間接下流でもある。カットオフの BFS が「上流がまだ
/// 確定していないので後回し」にした 4 を訪問済み扱いにすると、3 が Clean に
/// なった後の再訪が捨てられ、4 だけが取り残されて再計算される。
#[test]
fn a_join_point_is_not_left_behind_by_the_cutoff() {
    let mut model = straight_chain(4); // 0 → 1 → 2 → 3
    model.add(OracleItem {
        provides: vec![4],
        requires: vec![1, 3], // 短い枝と長い枝の合流
    });
    model.evaluate(1);
    assert_eq!(model.clean_indices().len(), 5);

    model.touch(0).expect("touch");
    let reevaluated = model.evaluate_indices(1);

    assert_eq!(
        reevaluated,
        vec![0, 1],
        "合流点 4 は再計算されない（値は変わっていない）"
    );
    assert_eq!(model.clean_indices().len(), 5, "5 件すべてが Clean に戻る");
    assert!(model.kernel.check_invariants().is_empty());
}

// ---------------------------------------------------------------------
// 生成器の見張り
// ---------------------------------------------------------------------

/// 生成器が空振りしていないことを確認する。
///
/// このテストがないと、性質テストが「何も起きていない状態」ばかりを 1000 回
/// 検証して green になっていても気づけない。実際、層状 DAG を足す前は、
/// 間接下流を持つケースが 1000 中 5 件しかなかった。
#[test]
fn generators_cover_interesting_states() {
    let counters: Vec<(&str, Cell<u32>)> = [
        "total",
        "cycle",
        "unresolved",
        "ambiguous",
        "direct_downstream",
        "indirect_downstream",
    ]
    .into_iter()
    .map(|name| (name, Cell::new(0u32)))
    .collect();
    let bump = |name: &str| {
        for (candidate, count) in &counters {
            if *candidate == name {
                count.set(count.get() + 1);
            }
        }
    };

    // 乱数任せにすると、閾値の近くで通ったり落ちたりする。見張り役が揺れては
    // 意味がないので、種を固定して再現可能にする。
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 1000,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::deterministic_rng(RngAlgorithm::ChaCha),
    );
    runner
        .run(
            &(workspace_strategy(), 0usize..64),
            |((capabilities, items), seed)| {
                bump("total");
                let mut model = build(capabilities, items);
                model.evaluate(1);

                let alive = model.oracle.alive();
                if alive.iter().any(|index| model.oracle.is_in_cycle(*index)) {
                    bump("cycle");
                }
                if alive.iter().any(|index| model.oracle.is_unresolved(*index)) {
                    bump("unresolved");
                }
                if alive.iter().any(|index| {
                    model
                        .entity(*index)
                        .and_then(|entity| model.kernel.resolution(entity).ok())
                        .map(|resolution| resolution.is_ambiguous())
                        .unwrap_or(false)
                }) {
                    bump("ambiguous");
                }

                let clean = model.clean_indices();
                if let Some(target) = pick(&clean, seed) {
                    let reachable: BTreeSet<usize> = model
                        .oracle
                        .stale_after_change(target)
                        .into_iter()
                        .filter(|index| clean.contains(index))
                        .collect();
                    let direct: BTreeSet<usize> =
                        model.oracle.downstream(target).into_iter().collect();
                    if !reachable.is_empty() {
                        bump("direct_downstream");
                    }
                    if reachable.difference(&direct).next().is_some() {
                        bump("indirect_downstream");
                    }
                }
                Ok(())
            },
        )
        .expect("生成のみ。失敗する経路はない");

    let count = |name: &str| {
        counters
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, value)| value.get())
            .expect("既知のカウンタ")
    };
    let report: Vec<String> = counters
        .iter()
        .map(|(name, value)| format!("{name}={}", value.get()))
        .collect();
    let report = report.join(" ");

    // 閾値は、種を固定したときの実測（cycle=270 unresolved=309 ambiguous=208
    // direct=279 indirect=86）に対して余裕を取ってある。この見張りの役目は
    // 特定の比率を強制することではなく、**カバレッジの崩壊を検出すること**で
    // ある。層状 DAG を足す前、間接下流は 1000 中 5 件しかなかった。
    assert_eq!(count("total"), 1000, "{report}");
    assert!(count("cycle") >= 150, "循環を踏む頻度が低すぎる: {report}");
    assert!(
        count("unresolved") >= 150,
        "未解決を踏む頻度が低すぎる: {report}"
    );
    assert!(
        count("ambiguous") >= 100,
        "曖昧参照を踏む頻度が低すぎる: {report}"
    );
    assert!(
        count("direct_downstream") >= 150,
        "伝播が起きるケースが少なすぎる: {report}"
    );
    assert!(
        count("indirect_downstream") >= 50,
        "間接下流を持つケースが少なすぎる。カットオフの精度が測れていない: {report}"
    );
}
