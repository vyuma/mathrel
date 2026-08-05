//! カーネル本体（ファサード）。
//!
//! カーネルが知っているのは 2 つだけである。
//!
//! 1. 誰が誰に依存しているか
//! 2. いま何が古いか
//!
//! 評価はしない。パースもしない。数式の意味も解釈しない。計算はホストが行い、
//! 結果の指紋を [`Kernel::commit_evaluation`] で報告する（企画書 ADR-004）。
//! この線引きにより、カーネルは純粋・同期・I/O なしになり、素朴実装（オラクル）
//! との等価性テストが書けるようになる。

use crate::component::{ComponentStore, Decl, DisplayHint, Expr, MapStore, Source, TypeInfo};
use crate::entity::{Entity, EntityAllocator};
use crate::error::{KernelError, KernelResult};
use crate::graph::DependencyGraph;
use crate::relation::Capability;
use crate::state::{Digest, EvalState, Freshness, Resolution, Revision};
use crate::symbol::{Symbol, SymbolInterner};
use std::collections::{BTreeSet, VecDeque};

/// エンティティ登録の仕様。
#[derive(Clone, Debug, Default)]
pub struct ItemSpec {
    /// 宣言。
    pub decl: Option<Decl>,
    /// ホストが供給する IR。
    pub expr: Option<Expr>,
    /// 提供する Capability。
    pub provides: Vec<Capability>,
    /// 要求する Capability。
    pub requires: Vec<Capability>,
    /// 型情報。
    pub type_info: Option<TypeInfo>,
    /// 表示ヒント。
    pub display: Option<DisplayHint>,
    /// 原文字列。
    pub source: Option<String>,
}

/// 値の更新仕様。
#[derive(Clone, Debug, Default)]
pub struct ValueUpdate {
    /// 新しい IR。`None` なら据え置き。
    pub expr: Option<Expr>,
    /// 新しい provides。`None` なら据え置き。
    pub provides: Option<Vec<Capability>>,
    /// 新しい requires。`None` なら据え置き。
    pub requires: Option<Vec<Capability>>,
    /// 新しい原文字列。`None` なら据え置き。
    pub source: Option<String>,
    /// 新しい宣言。`None` なら据え置き。
    pub decl: Option<Decl>,
}

/// ホストからの評価結果報告。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvalOutcome {
    /// 評価に成功した。`digest` は結果の指紋。
    Value {
        /// 評価結果の指紋。生成はホストの責務。
        digest: Digest,
    },
    /// 評価に失敗した。
    Failed {
        /// 失敗理由。カーネルは解釈しない。
        reason: String,
    },
}

/// 変更操作が引き起こした影響の報告。
///
/// すべて差分である（新規に該当したものだけ）。順序は Entity の index 昇順に
/// 正規化されている。スナップショットテストの主要な検証対象になるため。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeReport {
    /// この操作後のリビジョン。
    pub revision: Revision,
    /// 新たに `Dirty` になったもの。
    pub newly_dirty: Vec<Entity>,
    /// 新たに `MaybeDirty` になったもの。
    pub newly_maybe_dirty: Vec<Entity>,
    /// 早期カットオフにより `Clean` に戻ったもの。
    pub newly_clean: Vec<Entity>,
    /// 新たに未解決になったもの。
    pub newly_unresolved: Vec<Entity>,
    /// 新たに解決されたもの。
    pub newly_resolved: Vec<Entity>,
    /// 新たに曖昧になったもの。
    pub newly_ambiguous: Vec<Entity>,
    /// 新たに形成された循環。
    pub cycles_formed: Vec<Vec<Entity>>,
    /// 解消された循環。
    pub cycles_broken: Vec<Vec<Entity>>,
}

impl ChangeReport {
    fn normalize(&mut self) {
        for list in [
            &mut self.newly_dirty,
            &mut self.newly_maybe_dirty,
            &mut self.newly_clean,
            &mut self.newly_unresolved,
            &mut self.newly_resolved,
            &mut self.newly_ambiguous,
        ] {
            list.sort_unstable();
            list.dedup();
        }
        self.cycles_formed.sort();
        self.cycles_broken.sort();
    }

    /// 何も変わらなかったか。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.newly_dirty.is_empty()
            && self.newly_maybe_dirty.is_empty()
            && self.newly_clean.is_empty()
            && self.newly_unresolved.is_empty()
            && self.newly_resolved.is_empty()
            && self.newly_ambiguous.is_empty()
            && self.cycles_formed.is_empty()
            && self.cycles_broken.is_empty()
    }
}

/// Relational Math Kernel。
#[derive(Debug)]
pub struct Kernel {
    allocator: EntityAllocator,
    interner: SymbolInterner,
    graph: DependencyGraph,
    decls: MapStore<Decl>,
    exprs: MapStore<Expr>,
    types: MapStore<TypeInfo>,
    displays: MapStore<DisplayHint>,
    sources: MapStore<Source>,
    evals: MapStore<EvalState>,
    revision: Revision,
    early_cutoff: bool,
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel {
    /// 空のカーネルを作る。
    ///
    /// ```
    /// use mathrel_kernel::Kernel;
    /// let kernel = Kernel::new();
    /// assert_eq!(kernel.entities().len(), 0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            allocator: EntityAllocator::new(),
            interner: SymbolInterner::new(),
            graph: DependencyGraph::new(),
            decls: MapStore::new(),
            exprs: MapStore::new(),
            types: MapStore::new(),
            displays: MapStore::new(),
            sources: MapStore::new(),
            evals: MapStore::new(),
            revision: Revision(0),
            early_cutoff: true,
        }
    }

    /// 早期カットオフの有効・無効を切り替える。
    ///
    /// 無効にすると、上流の値が変わらなくても下流をすべて再計算する。
    /// 素朴実装との等価性を確認する性質テスト（P1）でのみ使う。
    pub fn set_early_cutoff(&mut self, enabled: bool) {
        self.early_cutoff = enabled;
    }

    /// 文字列を [`Symbol`] に変換する。
    pub fn intern(&mut self, text: &str) -> Symbol {
        self.interner.intern(text)
    }

    /// [`Symbol`] を文字列に戻す。
    #[must_use]
    pub fn symbol_name(&self, symbol: Symbol) -> &str {
        self.interner.name(symbol)
    }

    /// 現在のリビジョン。
    #[must_use]
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// 生存中の Entity を index 昇順で返す。
    #[must_use]
    pub fn entities(&self) -> Vec<Entity> {
        self.allocator.iter_alive().collect()
    }

    // ---------------------------------------------------------------
    // 変更操作
    // ---------------------------------------------------------------

    /// エンティティを登録する。
    ///
    /// ```
    /// use mathrel_kernel::{Capability, Kernel, ItemSpec};
    ///
    /// let mut kernel = Kernel::new();
    /// let x_name = kernel.intern("x");
    /// let (x, _) = kernel
    ///     .add_item(ItemSpec {
    ///         provides: vec![Capability::NameBound(x_name)],
    ///         ..Default::default()
    ///     })
    ///     .expect("add");
    /// assert!(kernel.resolution(x).expect("resolution").is_resolved());
    /// ```
    pub fn add_item(&mut self, spec: ItemSpec) -> KernelResult<(Entity, ChangeReport)> {
        let entity = self.allocator.allocate();
        self.revision = self.revision.next();

        if let Some(decl) = spec.decl {
            self.decls.set(entity, decl);
        }
        if let Some(expr) = spec.expr {
            self.exprs.set(entity, expr);
        }
        if let Some(type_info) = spec.type_info {
            self.types.set(entity, type_info);
        }
        if let Some(display) = spec.display {
            self.displays.set(entity, display);
        }
        if let Some(source) = spec.source {
            self.sources.set(entity, Source(source));
        }
        self.evals.set(entity, EvalState::new());

        let affected = self
            .graph
            .set_relations(entity, spec.provides, spec.requires);

        let mut report = ChangeReport {
            revision: self.revision,
            ..Default::default()
        };
        self.settle(affected, &mut report);
        report.normalize();
        Ok((entity, report))
    }

    /// エンティティを削除する。
    ///
    /// 依存していたエンティティは削除されず、未解決状態になる。
    /// 「未完成のまま保持する」ことがワークスペースの前提だからである。
    pub fn remove_item(&mut self, entity: Entity) -> KernelResult<ChangeReport> {
        self.check_alive(entity)?;
        self.revision = self.revision.next();

        let affected = self.graph.remove_entity(entity);
        self.decls.remove(entity);
        self.exprs.remove(entity);
        self.types.remove(entity);
        self.displays.remove(entity);
        self.sources.remove(entity);
        self.evals.remove(entity);
        self.allocator.deallocate(entity);

        let mut report = ChangeReport {
            revision: self.revision,
            ..Default::default()
        };
        self.settle(affected.clone(), &mut report);
        for target in &affected {
            if self.allocator.is_alive(*target) {
                self.propagate_from(*target, &mut report);
            }
        }
        report.normalize();
        Ok(report)
    }

    /// 値・IR・関係を差し替える。
    pub fn change_value(
        &mut self,
        entity: Entity,
        update: ValueUpdate,
    ) -> KernelResult<ChangeReport> {
        self.check_alive(entity)?;
        self.revision = self.revision.next();

        if let Some(expr) = update.expr {
            self.exprs.set(entity, expr);
        }
        if let Some(source) = update.source {
            self.sources.set(entity, Source(source));
        }
        if let Some(decl) = update.decl {
            self.decls.set(entity, decl);
        }

        let relations_changed = update.provides.is_some() || update.requires.is_some();
        let mut affected = BTreeSet::new();
        affected.insert(entity);
        if relations_changed {
            let provides = update
                .provides
                .unwrap_or_else(|| self.graph.provides_of(entity).to_vec());
            let requires = update
                .requires
                .unwrap_or_else(|| self.graph.requires_of(entity).to_vec());
            affected = self.graph.set_relations(entity, provides, requires);
        }

        let mut report = ChangeReport {
            revision: self.revision,
            ..Default::default()
        };
        self.settle(affected, &mut report);
        self.mark_dirty(entity, &mut report);
        self.propagate_from(entity, &mut report);
        report.normalize();
        Ok(report)
    }

    // ---------------------------------------------------------------
    // 評価サイクル
    // ---------------------------------------------------------------

    /// 今すぐ再計算可能なエンティティを返す。
    ///
    /// 条件は、再計算が必要で、循環に含まれず、未解決でなく、直近の評価に
    /// 失敗しておらず、かつ**全上流が `Clean`** であること。
    ///
    /// SPEC-GAP: 要件定義書 §6 の記述は `Dirty` のみを候補としていたが、
    /// それでは新規追加された `NeverEvaluated` のエンティティが永久に
    /// スケジュールされず、性質 P2（全エンティティが Clean に到達する）が
    /// 成立しない。`NeverEvaluated` と `MaybeDirty` も候補に含めた。
    /// `MaybeDirty` は早期カットオフが commit 時に先に解決するため、
    /// ここに現れるのは伝播が途切れた場合の保険であり、収束（P3）を保証する。
    #[must_use]
    pub fn next_batch(&self) -> Vec<Entity> {
        let mut batch: Vec<Entity> = self
            .allocator
            .iter_alive()
            .filter(|entity| self.is_schedulable(*entity))
            .collect();
        batch.sort_unstable();
        batch
    }

    /// ホストが評価を終えたことを報告する。
    ///
    /// 指紋が前回と同一なら早期カットオフが働き、下流の `MaybeDirty` が
    /// `Clean` に戻る。
    pub fn commit_evaluation(
        &mut self,
        entity: Entity,
        outcome: EvalOutcome,
    ) -> KernelResult<ChangeReport> {
        self.check_alive(entity)?;
        if self.graph.is_in_cycle(entity) {
            return Err(KernelError::CommitOnCyclic(entity));
        }
        if self.graph.resolution(entity).is_unresolved() {
            return Err(KernelError::CommitOnUnresolved(entity));
        }
        if matches!(self.freshness_of(entity), Freshness::Clean { .. }) {
            return Err(KernelError::CommitOnClean(entity));
        }

        self.revision = self.revision.next();
        let mut report = ChangeReport {
            revision: self.revision,
            ..Default::default()
        };

        match outcome {
            EvalOutcome::Failed { reason } => {
                // 失敗したエンティティは `Dirty` のまま失敗理由を持つ。
                // 同じ入力で再スケジュールしても同じ結果になるため、
                // 何かが変わるまで `next_batch` から外す。
                if let Some(state) = self.evals.get_mut(entity) {
                    state.freshness = Freshness::Dirty;
                    state.failure = Some(reason);
                }
            }
            EvalOutcome::Value { digest } => {
                let previous = self
                    .evals
                    .get(entity)
                    .and_then(|state| state.last_clean)
                    .map(|(_, digest)| digest);
                let revision = self.revision;
                if let Some(state) = self.evals.get_mut(entity) {
                    state.freshness = Freshness::Clean {
                        at_revision: revision,
                        digest,
                    };
                    state.last_clean = Some((revision, digest));
                    state.failure = None;
                }

                let unchanged = previous == Some(digest);
                if unchanged && self.early_cutoff {
                    self.cutoff_from(entity, &mut report);
                } else {
                    self.promote_downstream(entity, &mut report);
                }
            }
        }

        report.normalize();
        Ok(report)
    }

    /// すべてのエンティティを収束するまで評価する。
    ///
    /// `evaluate` はホストの評価器である。カーネルは中身を知らない。
    /// 戻り値は評価した順序。
    ///
    /// 進捗がなくなった時点で停止するため、必ず有限回で終わる。
    pub fn run_to_fixpoint<F>(&mut self, mut evaluate: F) -> Vec<Entity>
    where
        F: FnMut(&Kernel, Entity) -> EvalOutcome,
    {
        let mut order = Vec::new();
        loop {
            let batch = self.next_batch();
            if batch.is_empty() {
                break;
            }
            for entity in batch {
                if !self.is_schedulable(entity) {
                    continue;
                }
                let outcome = evaluate(self, entity);
                if self.commit_evaluation(entity, outcome).is_ok() {
                    order.push(entity);
                }
            }
        }
        order
    }

    // ---------------------------------------------------------------
    // 問い合わせ
    // ---------------------------------------------------------------

    /// 参照解決の状態。
    pub fn resolution(&self, entity: Entity) -> KernelResult<&Resolution> {
        self.check_alive(entity)?;
        Ok(self.graph.resolution(entity))
    }

    /// 鮮度の状態。
    pub fn freshness(&self, entity: Entity) -> KernelResult<Freshness> {
        self.check_alive(entity)?;
        Ok(self.freshness_of(entity))
    }

    /// 直近の評価失敗の理由。
    pub fn failure(&self, entity: Entity) -> KernelResult<Option<&str>> {
        self.check_alive(entity)?;
        Ok(self
            .evals
            .get(entity)
            .and_then(|state| state.failure.as_deref()))
    }

    /// 上流（このエンティティが依存している先）。
    pub fn dependencies(&self, entity: Entity) -> KernelResult<Vec<Entity>> {
        self.check_alive(entity)?;
        Ok(self.graph.upstream(entity))
    }

    /// 下流（このエンティティに依存しているもの）。
    pub fn dependents(&self, entity: Entity) -> KernelResult<Vec<Entity>> {
        self.check_alive(entity)?;
        Ok(self.graph.downstream(entity))
    }

    /// 循環に含まれるか。
    pub fn in_cycle(&self, entity: Entity) -> KernelResult<bool> {
        self.check_alive(entity)?;
        Ok(self.graph.is_in_cycle(entity))
    }

    /// 検出済みの循環（SCC 単位）。
    #[must_use]
    pub fn cycles(&self) -> Vec<Vec<Entity>> {
        self.graph.cycles().to_vec()
    }

    /// 未解決の Capability を持つエンティティ一覧。
    #[must_use]
    pub fn unresolved(&self) -> Vec<(Entity, Vec<Capability>)> {
        self.allocator
            .iter_alive()
            .filter_map(|entity| {
                let resolution = self.graph.resolution(entity);
                if resolution.is_unresolved() {
                    Some((entity, resolution.missing().to_vec()))
                } else {
                    None
                }
            })
            .collect()
    }

    /// 再計算が必要なエンティティ一覧（`Dirty` と `MaybeDirty` の両方）。
    ///
    /// 素朴実装との等価性を検証する性質テスト（P1）で使う。
    #[must_use]
    pub fn stale_set(&self) -> BTreeSet<Entity> {
        self.allocator
            .iter_alive()
            .filter(|entity| {
                matches!(
                    self.freshness_of(*entity),
                    Freshness::Dirty | Freshness::MaybeDirty
                )
            })
            .collect()
    }

    /// 宣言。
    #[must_use]
    pub fn decl(&self, entity: Entity) -> Option<&Decl> {
        self.decls.get(entity)
    }

    /// ホストが供給した IR。
    #[must_use]
    pub fn expr(&self, entity: Entity) -> Option<&Expr> {
        self.exprs.get(entity)
    }

    /// 型情報。
    #[must_use]
    pub fn type_info(&self, entity: Entity) -> Option<&TypeInfo> {
        self.types.get(entity)
    }

    /// 表示ヒント。
    #[must_use]
    pub fn display_hint(&self, entity: Entity) -> Option<&DisplayHint> {
        self.displays.get(entity)
    }

    /// 原文字列。
    #[must_use]
    pub fn source(&self, entity: Entity) -> Option<&str> {
        self.sources.get(entity).map(|source| source.0.as_str())
    }

    /// 提供している Capability。
    #[must_use]
    pub fn provides(&self, entity: Entity) -> &[Capability] {
        self.graph.provides_of(entity)
    }

    /// 要求している Capability。
    #[must_use]
    pub fn requires(&self, entity: Entity) -> &[Capability] {
        self.graph.requires_of(entity)
    }

    /// Capability を人間が読める文字列にする。
    #[must_use]
    pub fn capability_label(&self, capability: Capability) -> String {
        match capability {
            Capability::NameBound(name) => format!("NameBound({})", self.interner.name(name)),
            Capability::FunctionBound { name, arity } => {
                format!("FunctionBound({}/{})", self.interner.name(name), arity)
            }
            Capability::TypeKnown(name) => format!("TypeKnown({})", self.interner.name(name)),
            Capability::PropositionProved(name) => {
                format!("PropositionProved({})", self.interner.name(name))
            }
        }
    }

    // ---------------------------------------------------------------
    // 内部
    // ---------------------------------------------------------------

    fn check_alive(&self, entity: Entity) -> KernelResult<()> {
        if self.allocator.is_alive(entity) {
            Ok(())
        } else if self.allocator.is_stale(entity) {
            Err(KernelError::StaleEntity(entity))
        } else {
            Err(KernelError::UnknownEntity(entity))
        }
    }

    fn freshness_of(&self, entity: Entity) -> Freshness {
        self.evals
            .get(entity)
            .map(|state| state.freshness)
            .unwrap_or_default()
    }

    fn is_schedulable(&self, entity: Entity) -> bool {
        if !self.allocator.is_alive(entity) {
            return false;
        }
        if self.graph.is_in_cycle(entity) {
            return false;
        }
        if self.graph.resolution(entity).is_unresolved() {
            return false;
        }
        let state = match self.evals.get(entity) {
            Some(state) => state,
            None => return false,
        };
        if state.failure.is_some() {
            return false;
        }
        if !state.freshness.needs_evaluation() {
            return false;
        }
        self.graph
            .upstream(entity)
            .into_iter()
            .all(|provider| self.freshness_of(provider).is_settled())
    }

    /// 影響を受けたエンティティを再解決し、循環を数え直す。
    fn settle(&mut self, affected: BTreeSet<Entity>, report: &mut ChangeReport) {
        let mut became_stale: Vec<Entity> = Vec::new();

        for entity in affected {
            if !self.allocator.is_alive(entity) {
                continue;
            }
            let before = self.graph.resolution(entity).clone();
            self.graph.resolve(entity);
            let after = self.graph.resolution(entity).clone();
            if before == after {
                continue;
            }

            // 解決状態が少しでも変わったら古くする。**種別の変化だけを見ては
            // ならない。** provider が 2 件から 3 件に増えた場合、種別は
            // `Ambiguous` のまま変わらないが、上流の集合には新しい辺が増えて
            // いる。ここで見落とすと、新たな上流を獲得した（場合によっては
            // 循環に入った）エンティティが `Clean` を名乗り続ける。
            became_stale.push(entity);

            if after.is_unresolved() && !before.is_unresolved() {
                report.newly_unresolved.push(entity);
            }
            if !after.is_unresolved() && before.is_unresolved() {
                report.newly_resolved.push(entity);
            }
            if after.is_ambiguous() && !before.is_ambiguous() {
                report.newly_ambiguous.push(entity);
            }
        }

        let entities = self.entities();
        let previous_cycles = self.graph.recompute_cycles(&entities);
        let current_cycles = self.graph.cycles().to_vec();
        for cycle in &current_cycles {
            if !previous_cycles.contains(cycle) {
                report.cycles_formed.push(cycle.clone());
            }
        }
        for cycle in &previous_cycles {
            if !current_cycles.contains(cycle) {
                report.cycles_broken.push(cycle.clone());
            }
        }

        for entity in became_stale {
            self.mark_dirty(entity, report);
            self.propagate_from(entity, report);
        }
    }

    /// 変更点から下流へ dirty を伝播する。
    ///
    /// 直接下流は `Dirty`、間接下流は `MaybeDirty` にする。この 2 段階が
    /// 早期カットオフの前提であり、偽陽性を抑える唯一の手段である。
    fn propagate_from(&mut self, source: Entity, report: &mut ChangeReport) {
        let direct: BTreeSet<Entity> = self.graph.downstream(source).into_iter().collect();
        for entity in &direct {
            self.mark_dirty(*entity, report);
        }
        for entity in self.graph.reachable_downstream(source) {
            if direct.contains(&entity) {
                continue;
            }
            self.mark_maybe_dirty(entity, report);
        }
    }

    fn mark_dirty(&mut self, entity: Entity, report: &mut ChangeReport) {
        if !self.allocator.is_alive(entity) {
            return;
        }
        let state = match self.evals.get_mut(entity) {
            Some(state) => state,
            None => return,
        };
        if matches!(state.freshness, Freshness::Dirty) {
            // 鮮度は変わらないが、失敗の記録は落とす。`commit_evaluation` が
            // 失敗を保持するのは「入力が同じなら結果も同じ」だからであり、
            // ここに来たということは入力側で何かが変わっている。落とさないと
            // `is_schedulable` が弾き続け、失敗したエンティティを二度と
            // 再計算できなくなる（式を直しても直らない）。
            state.failure = None;
            return;
        }
        if let Freshness::Clean {
            at_revision,
            digest,
        } = state.freshness
        {
            state.last_clean = Some((at_revision, digest));
        }
        let was_never_evaluated = matches!(state.freshness, Freshness::NeverEvaluated);
        state.freshness = Freshness::Dirty;
        state.failure = None;
        if !was_never_evaluated {
            report.newly_dirty.push(entity);
        }
    }

    fn mark_maybe_dirty(&mut self, entity: Entity, report: &mut ChangeReport) {
        if !self.allocator.is_alive(entity) {
            return;
        }
        let state = match self.evals.get_mut(entity) {
            Some(state) => state,
            None => return,
        };
        if let Freshness::Clean {
            at_revision,
            digest,
        } = state.freshness
        {
            state.last_clean = Some((at_revision, digest));
            state.freshness = Freshness::MaybeDirty;
            state.failure = None;
            report.newly_maybe_dirty.push(entity);
        }
    }

    /// 指紋が変わったときの下流昇格。
    fn promote_downstream(&mut self, source: Entity, report: &mut ChangeReport) {
        let targets: Vec<Entity> = if self.early_cutoff {
            self.graph.downstream(source)
        } else {
            // カットオフ無効時は到達可能な下流をすべて `Dirty` にする。
            // 素朴実装（オラクル）と同じ振る舞いになる。
            self.graph.reachable_downstream(source).into_iter().collect()
        };
        for entity in targets {
            if matches!(self.freshness_of(entity), Freshness::MaybeDirty) {
                self.mark_dirty(entity, report);
            }
        }
    }

    /// 指紋が変わらなかったときのカットオフ伝播。
    ///
    /// `MaybeDirty` を `Clean` に戻す。ただし戻せるのは**全上流が `Clean`**
    /// のときだけである。この条件がないと「Clean なエンティティの上流が
    /// Dirty」という状態が生まれ、不変条件が壊れる。
    fn cutoff_from(&mut self, source: Entity, report: &mut ChangeReport) {
        let mut queue: VecDeque<Entity> = self.graph.downstream(source).into_iter().collect();
        let mut visited: BTreeSet<Entity> = BTreeSet::new();
        let revision = self.revision;

        while let Some(entity) = queue.pop_front() {
            if !matches!(self.freshness_of(entity), Freshness::MaybeDirty) {
                continue;
            }
            let upstream_settled = self
                .graph
                .upstream(entity)
                .into_iter()
                .all(|provider| self.freshness_of(provider).is_settled());
            if !upstream_settled {
                // まだ戻せない。合流点（複数の上流を持つエンティティ）は、
                // 先に別の上流が Clean になった時点で改めてキューに入る。
                //
                // ここで訪問済みにしてはならない。訪問済みにすると、その
                // 再訪が捨てられ、実際には値が変わっていないのに MaybeDirty
                // のまま取り残される。それは偽陽性そのものである。
                continue;
            }
            if !visited.insert(entity) {
                continue;
            }
            let restored = self
                .evals
                .get(entity)
                .and_then(|state| state.last_clean)
                .map(|(_, digest)| digest);
            let digest = match restored {
                Some(digest) => digest,
                None => continue,
            };
            if let Some(state) = self.evals.get_mut(entity) {
                state.freshness = Freshness::Clean {
                    at_revision: revision,
                    digest,
                };
                state.last_clean = Some((revision, digest));
            }
            report.newly_clean.push(entity);
            for next in self.graph.downstream(entity) {
                queue.push_back(next);
            }
        }
    }

    /// 不変条件の検証。デバッグビルドとテストでのみ呼ぶ。
    ///
    /// `Clean` なエンティティの全上流は `Clean` または `NeverEvaluated`
    /// でなければならない。破れていれば違反したエンティティを返す。
    #[must_use]
    pub fn check_invariants(&self) -> Vec<(Entity, Entity)> {
        let mut violations = Vec::new();
        for entity in self.allocator.iter_alive() {
            if !matches!(self.freshness_of(entity), Freshness::Clean { .. }) {
                continue;
            }
            for provider in self.graph.upstream(entity) {
                if !matches!(
                    self.freshness_of(provider),
                    Freshness::Clean { .. } | Freshness::NeverEvaluated
                ) {
                    violations.push((entity, provider));
                }
            }
        }
        violations
    }
}
