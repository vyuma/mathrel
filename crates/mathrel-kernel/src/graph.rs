//! 依存グラフの増分保守と循環検出。
//!
//! 依存グラフは一次データではない。`requires` / `provides` から導出される
//! 派生ビューである（企画書 ADR-003）。ただし**導出であることと、毎回
//! 再構築することは別**であり、編集のたびに全再構築すると編集コストが
//! O(n) になる。ここでは Capability ごとの逆引き索引を持ち、影響を受ける
//! エンティティだけを再解決する。
//!
//! 集合には `BTreeSet` を使う。反復順序が決定的でないと `ChangeReport` の
//! 内容が実行ごとに変わり、性質テスト P5（決定性）が成立しないため。

use crate::entity::Entity;
use crate::relation::Capability;
use crate::state::Resolution;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::collections::{BTreeSet, VecDeque};

/// ある Capability の provider 群。多くの場合 1 件なのでインライン化する。
type Providers = SmallVec<[Entity; 1]>;
/// ある Capability の requirer 群。
type Requirers = SmallVec<[Entity; 2]>;

/// 依存グラフ。
#[derive(Debug, Default, Clone)]
pub struct DependencyGraph {
    provides: FxHashMap<Entity, Vec<Capability>>,
    requires: FxHashMap<Entity, Vec<Capability>>,
    provider_index: FxHashMap<Capability, Providers>,
    requirer_index: FxHashMap<Capability, Requirers>,
    /// e が依存している provider 群（上流）。
    upstream: FxHashMap<Entity, BTreeSet<Entity>>,
    /// e に依存している requirer 群（下流）。
    downstream: FxHashMap<Entity, BTreeSet<Entity>>,
    resolution: FxHashMap<Entity, Resolution>,
    in_cycle: FxHashSet<Entity>,
    sccs: Vec<Vec<Entity>>,
}

impl DependencyGraph {
    /// 空のグラフ。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// エンティティの関係を登録または差し替える。
    ///
    /// 戻り値は、この操作によって解決状態が変わり得るエンティティの集合。
    /// 呼び出し側はこれらを [`Self::resolve`] で再解決する。
    pub fn set_relations(
        &mut self,
        entity: Entity,
        provides: Vec<Capability>,
        requires: Vec<Capability>,
    ) -> BTreeSet<Entity> {
        let mut affected = BTreeSet::new();
        affected.insert(entity);

        // provides の差分。追加・削除された Capability を requires している
        // エンティティは、解決先が変わる可能性がある。
        let old_provides = self.provides.remove(&entity).unwrap_or_default();
        for capability in &old_provides {
            self.detach_provider(*capability, entity);
        }
        for capability in old_provides.iter().chain(provides.iter()) {
            for requirer in self.requirers_of(*capability) {
                affected.insert(requirer);
            }
        }
        for capability in &provides {
            self.attach_provider(*capability, entity);
        }
        // attach 後にもう一度拾う。新規 Capability の requirer は
        // attach 前後で変わらないが、記述の対称性のため両方で拾っておく。
        for capability in &provides {
            for requirer in self.requirers_of(*capability) {
                affected.insert(requirer);
            }
        }
        self.provides.insert(entity, provides);

        // requires の差分。索引を張り替える。
        let old_requires = self.requires.remove(&entity).unwrap_or_default();
        for capability in &old_requires {
            self.detach_requirer(*capability, entity);
        }
        for capability in &requires {
            self.attach_requirer(*capability, entity);
        }
        self.requires.insert(entity, requires);

        affected
    }

    /// エンティティをグラフから取り除く。
    ///
    /// 戻り値は再解決が必要なエンティティの集合（削除されたエンティティ自身は含まない）。
    pub fn remove_entity(&mut self, entity: Entity) -> BTreeSet<Entity> {
        let mut affected = BTreeSet::new();

        let provides = self.provides.remove(&entity).unwrap_or_default();
        for capability in &provides {
            self.detach_provider(*capability, entity);
            for requirer in self.requirers_of(*capability) {
                if requirer != entity {
                    affected.insert(requirer);
                }
            }
        }

        let requires = self.requires.remove(&entity).unwrap_or_default();
        for capability in &requires {
            self.detach_requirer(*capability, entity);
        }

        // 上下流の辺を落とす。
        if let Some(providers) = self.upstream.remove(&entity) {
            for provider in providers {
                if let Some(set) = self.downstream.get_mut(&provider) {
                    set.remove(&entity);
                }
            }
        }
        if let Some(requirers) = self.downstream.remove(&entity) {
            for requirer in requirers {
                if let Some(set) = self.upstream.get_mut(&requirer) {
                    set.remove(&entity);
                }
                affected.insert(requirer);
            }
        }

        self.resolution.remove(&entity);
        self.in_cycle.remove(&entity);
        affected
    }

    /// 単一エンティティの参照解決をやり直し、上流の辺を張り替える。
    ///
    /// 解決状態が変化したときだけ `Some(旧状態)` を返す。
    pub fn resolve(&mut self, entity: Entity) -> Option<Resolution> {
        let requires = self.requires.get(&entity).cloned().unwrap_or_default();

        let mut new_upstream = BTreeSet::new();
        let mut missing = Vec::new();
        let mut ambiguous = Vec::new();

        for capability in requires {
            let providers = self
                .provider_index
                .get(&capability)
                .map(|providers| providers.as_slice())
                .unwrap_or_default();
            match providers.len() {
                0 => missing.push(capability),
                1 => {
                    new_upstream.insert(providers[0]);
                }
                _ => {
                    // 曖昧な場合も全 provider へ辺を張る。どちらが変わっても
                    // dirty が伝わるようにするため、健全側へ倒す。
                    let mut sorted: Vec<Entity> = providers.to_vec();
                    sorted.sort_unstable();
                    for provider in &sorted {
                        new_upstream.insert(*provider);
                    }
                    ambiguous.push((capability, sorted));
                }
            }
        }

        missing.sort_unstable();
        missing.dedup();
        ambiguous.sort_by(|a, b| a.0.cmp(&b.0));

        let new_resolution = if !missing.is_empty() {
            Resolution::Unresolved { missing, ambiguous }
        } else if !ambiguous.is_empty() {
            Resolution::Ambiguous {
                conflicts: ambiguous,
            }
        } else {
            Resolution::Resolved
        };

        // 上流の張り替え。
        let old_upstream = self.upstream.remove(&entity).unwrap_or_default();
        for provider in old_upstream.difference(&new_upstream) {
            if let Some(set) = self.downstream.get_mut(provider) {
                set.remove(&entity);
            }
        }
        for provider in new_upstream.difference(&old_upstream) {
            self.downstream.entry(*provider).or_default().insert(entity);
        }
        self.upstream.insert(entity, new_upstream);

        let previous = self.resolution.insert(entity, new_resolution.clone());
        match previous {
            Some(old) if old == new_resolution => None,
            Some(old) => Some(old),
            None if new_resolution.is_resolved() => None,
            None => Some(Resolution::Resolved),
        }
    }

    /// 解決状態を引く。
    #[must_use]
    pub fn resolution(&self, entity: Entity) -> &Resolution {
        self.resolution
            .get(&entity)
            .unwrap_or(&Resolution::Resolved)
    }

    /// 上流（このエンティティが依存している先）。
    #[must_use]
    pub fn upstream(&self, entity: Entity) -> Vec<Entity> {
        self.upstream
            .get(&entity)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// 下流（このエンティティに依存しているもの）。
    #[must_use]
    pub fn downstream(&self, entity: Entity) -> Vec<Entity> {
        self.downstream
            .get(&entity)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// 下流への到達可能集合（自分自身は含まない）。
    #[must_use]
    pub fn reachable_downstream(&self, entity: Entity) -> BTreeSet<Entity> {
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<Entity> = self.downstream(entity).into_iter().collect();
        while let Some(current) = queue.pop_front() {
            if current == entity || !seen.insert(current) {
                continue;
            }
            for next in self.downstream(current) {
                if !seen.contains(&next) {
                    queue.push_back(next);
                }
            }
        }
        seen
    }

    /// このエンティティが提供している Capability。
    #[must_use]
    pub fn provides_of(&self, entity: Entity) -> &[Capability] {
        self.provides
            .get(&entity)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// このエンティティが要求している Capability。
    #[must_use]
    pub fn requires_of(&self, entity: Entity) -> &[Capability] {
        self.requires
            .get(&entity)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// 循環に含まれるか。
    #[must_use]
    pub fn is_in_cycle(&self, entity: Entity) -> bool {
        self.in_cycle.contains(&entity)
    }

    /// 検出済みの循環（SCC 単位）。
    #[must_use]
    pub fn cycles(&self) -> &[Vec<Entity>] {
        &self.sccs
    }

    /// 循環を再計算する。旧 SCC 集合を返す。
    ///
    /// SPEC-GAP: 要件定義書 §13.3 — SCC は差分化せず全体再計算とした。
    /// 想定エンティティ数（〜10^4）では Tarjan の全体実行が十分速く、
    /// 差分化は正しさの検証コストに見合わない。P5 以降で再検討する。
    pub fn recompute_cycles(&mut self, entities: &[Entity]) -> Vec<Vec<Entity>> {
        let previous = std::mem::take(&mut self.sccs);
        self.in_cycle.clear();

        let mut components = tarjan_scc(entities, |entity| self.upstream(entity));
        components.retain(|component| {
            component.len() > 1 || {
                let node = component[0];
                self.upstream(node).contains(&node)
            }
        });
        for component in &mut components {
            component.sort_unstable();
            for entity in component.iter() {
                self.in_cycle.insert(*entity);
            }
        }
        components.sort();
        self.sccs = components;
        previous
    }

    fn requirers_of(&self, capability: Capability) -> Vec<Entity> {
        self.requirer_index
            .get(&capability)
            .map(|requirers| requirers.to_vec())
            .unwrap_or_default()
    }

    fn attach_provider(&mut self, capability: Capability, entity: Entity) {
        let providers = self.provider_index.entry(capability).or_default();
        if !providers.contains(&entity) {
            providers.push(entity);
            providers.sort_unstable();
        }
    }

    fn detach_provider(&mut self, capability: Capability, entity: Entity) {
        if let Some(providers) = self.provider_index.get_mut(&capability) {
            providers.retain(|candidate| *candidate != entity);
            if providers.is_empty() {
                self.provider_index.remove(&capability);
            }
        }
    }

    fn attach_requirer(&mut self, capability: Capability, entity: Entity) {
        let requirers = self.requirer_index.entry(capability).or_default();
        if !requirers.contains(&entity) {
            requirers.push(entity);
            requirers.sort_unstable();
        }
    }

    fn detach_requirer(&mut self, capability: Capability, entity: Entity) {
        if let Some(requirers) = self.requirer_index.get_mut(&capability) {
            requirers.retain(|candidate| *candidate != entity);
            if requirers.is_empty() {
                self.requirer_index.remove(&capability);
            }
        }
    }
}

/// Tarjan の強連結成分分解。反復版。
///
/// 再帰版は深い依存鎖でスタックを溢れさせる。数式の依存鎖が数千段になる
/// ことは考えにくいが、カーネルはパニックしない約束なので反復で書く。
fn tarjan_scc<F>(nodes: &[Entity], successors: F) -> Vec<Vec<Entity>>
where
    F: Fn(Entity) -> Vec<Entity>,
{
    #[derive(Clone, Copy)]
    struct NodeState {
        index: u32,
        lowlink: u32,
        on_stack: bool,
    }

    let mut state: FxHashMap<Entity, NodeState> = FxHashMap::default();
    let mut stack: Vec<Entity> = Vec::new();
    let mut components: Vec<Vec<Entity>> = Vec::new();
    let mut next_index: u32 = 0;
    let node_set: FxHashSet<Entity> = nodes.iter().copied().collect();

    for &root in nodes {
        if state.contains_key(&root) {
            continue;
        }

        // (ノード, 後続リスト, 次に見る後続の位置)
        let mut call_stack: Vec<(Entity, Vec<Entity>, usize)> = Vec::new();
        state.insert(
            root,
            NodeState {
                index: next_index,
                lowlink: next_index,
                on_stack: true,
            },
        );
        next_index += 1;
        stack.push(root);
        call_stack.push((root, successors(root), 0));

        while let Some((node, edges, cursor)) = call_stack.last_mut() {
            let node = *node;
            if *cursor < edges.len() {
                let next = edges[*cursor];
                *cursor += 1;
                if !node_set.contains(&next) {
                    continue;
                }
                match state.get(&next).copied() {
                    None => {
                        state.insert(
                            next,
                            NodeState {
                                index: next_index,
                                lowlink: next_index,
                                on_stack: true,
                            },
                        );
                        next_index += 1;
                        stack.push(next);
                        call_stack.push((next, successors(next), 0));
                    }
                    Some(next_state) if next_state.on_stack => {
                        if let Some(node_state) = state.get_mut(&node) {
                            node_state.lowlink = node_state.lowlink.min(next_state.index);
                        }
                    }
                    Some(_) => {}
                }
            } else {
                let finished = node;
                let finished_state = state.get(&finished).copied();
                call_stack.pop();

                if let (Some(finished_state), Some((parent, _, _))) =
                    (finished_state, call_stack.last())
                {
                    let parent = *parent;
                    if let Some(parent_state) = state.get_mut(&parent) {
                        parent_state.lowlink = parent_state.lowlink.min(finished_state.lowlink);
                    }
                }

                if let Some(finished_state) = finished_state {
                    if finished_state.lowlink == finished_state.index {
                        let mut component = Vec::new();
                        while let Some(popped) = stack.pop() {
                            if let Some(popped_state) = state.get_mut(&popped) {
                                popped_state.on_stack = false;
                            }
                            component.push(popped);
                            if popped == finished {
                                break;
                            }
                        }
                        components.push(component);
                    }
                }
            }
        }
    }

    components
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityAllocator;
    use crate::symbol::SymbolInterner;

    fn setup() -> (EntityAllocator, SymbolInterner, DependencyGraph) {
        (
            EntityAllocator::new(),
            SymbolInterner::new(),
            DependencyGraph::new(),
        )
    }

    #[test]
    fn single_provider_produces_one_edge() {
        let (mut alloc, mut interner, mut graph) = setup();
        let x = alloc.allocate();
        let y = alloc.allocate();
        let cap = Capability::NameBound(interner.intern("x"));

        graph.set_relations(x, vec![cap], vec![]);
        graph.set_relations(y, vec![], vec![cap]);
        graph.resolve(x);
        graph.resolve(y);

        assert_eq!(graph.upstream(y), vec![x]);
        assert_eq!(graph.downstream(x), vec![y]);
        assert!(graph.resolution(y).is_resolved());
    }

    #[test]
    fn missing_provider_yields_unresolved() {
        let (mut alloc, mut interner, mut graph) = setup();
        let y = alloc.allocate();
        let cap = Capability::NameBound(interner.intern("x"));
        graph.set_relations(y, vec![], vec![cap]);
        graph.resolve(y);
        assert!(graph.resolution(y).is_unresolved());
        assert_eq!(graph.resolution(y).missing(), &[cap]);
    }

    #[test]
    fn two_providers_yield_ambiguous_with_edges_to_both() {
        let (mut alloc, mut interner, mut graph) = setup();
        let a = alloc.allocate();
        let b = alloc.allocate();
        let y = alloc.allocate();
        let cap = Capability::NameBound(interner.intern("x"));

        graph.set_relations(a, vec![cap], vec![]);
        graph.set_relations(b, vec![cap], vec![]);
        graph.set_relations(y, vec![], vec![cap]);
        graph.resolve(y);

        assert!(graph.resolution(y).is_ambiguous());
        assert_eq!(
            graph.upstream(y),
            vec![a, b],
            "健全側に倒して両方へ辺を張る"
        );
    }

    #[test]
    fn cycle_of_two_is_detected() {
        let (mut alloc, mut interner, mut graph) = setup();
        let a = alloc.allocate();
        let b = alloc.allocate();
        let cap_a = Capability::NameBound(interner.intern("a"));
        let cap_b = Capability::NameBound(interner.intern("b"));

        graph.set_relations(a, vec![cap_a], vec![cap_b]);
        graph.set_relations(b, vec![cap_b], vec![cap_a]);
        graph.resolve(a);
        graph.resolve(b);
        graph.recompute_cycles(&[a, b]);

        assert!(graph.is_in_cycle(a));
        assert!(graph.is_in_cycle(b));
        assert_eq!(graph.cycles(), &[vec![a, b]]);
    }

    #[test]
    fn self_loop_is_a_cycle() {
        let (mut alloc, mut interner, mut graph) = setup();
        let a = alloc.allocate();
        let cap = Capability::NameBound(interner.intern("a"));
        graph.set_relations(a, vec![cap], vec![cap]);
        graph.resolve(a);
        graph.recompute_cycles(&[a]);
        assert!(graph.is_in_cycle(a));
    }

    #[test]
    fn acyclic_chain_has_no_cycles() {
        let (mut alloc, mut interner, mut graph) = setup();
        let a = alloc.allocate();
        let b = alloc.allocate();
        let c = alloc.allocate();
        let cap_a = Capability::NameBound(interner.intern("a"));
        let cap_b = Capability::NameBound(interner.intern("b"));

        graph.set_relations(a, vec![cap_a], vec![]);
        graph.set_relations(b, vec![cap_b], vec![cap_a]);
        graph.set_relations(c, vec![], vec![cap_b]);
        graph.resolve(a);
        graph.resolve(b);
        graph.resolve(c);
        graph.recompute_cycles(&[a, b, c]);

        assert!(graph.cycles().is_empty());
        assert_eq!(graph.reachable_downstream(a), [b, c].into_iter().collect());
    }

    #[test]
    fn removing_provider_detaches_edges() {
        let (mut alloc, mut interner, mut graph) = setup();
        let x = alloc.allocate();
        let y = alloc.allocate();
        let cap = Capability::NameBound(interner.intern("x"));
        graph.set_relations(x, vec![cap], vec![]);
        graph.set_relations(y, vec![], vec![cap]);
        graph.resolve(y);

        let affected = graph.remove_entity(x);
        assert!(affected.contains(&y));
        graph.resolve(y);
        assert!(graph.resolution(y).is_unresolved());
        assert!(graph.upstream(y).is_empty());
    }
}
