//! テスト用の補助と、素朴な参照実装（オラクル）。

#![allow(dead_code)]

use mathrel_kernel::{
    Capability, Entity, EvalOutcome, ItemSpec, Kernel, KernelResult, ValueUpdate,
};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

// ---------------------------------------------------------------------
// 補助
// ---------------------------------------------------------------------

/// `NameBound(name)` を作る。
pub fn name_bound(kernel: &mut Kernel, name: &str) -> Capability {
    Capability::NameBound(kernel.intern(name))
}

/// `FunctionBound(name/arity)` を作る。
pub fn func_bound(kernel: &mut Kernel, name: &str, arity: u8) -> Capability {
    Capability::FunctionBound {
        name: kernel.intern(name),
        arity,
    }
}

/// `TypeKnown(name)` を作る。
pub fn type_known(kernel: &mut Kernel, name: &str) -> Capability {
    Capability::TypeKnown(kernel.intern(name))
}

/// 値定義を 1 つ足す。
pub fn add_value(kernel: &mut Kernel, name: &str, requires: Vec<Capability>) -> Entity {
    let provides = vec![name_bound(kernel, name)];
    kernel
        .add_item(ItemSpec {
            provides,
            requires,
            source: Some(format!("{name} = ...")),
            ..Default::default()
        })
        .expect("add_item は失敗しない")
        .0
}

/// 関数定義を 1 つ足す。
pub fn add_function(
    kernel: &mut Kernel,
    name: &str,
    arity: u8,
    requires: Vec<Capability>,
) -> Entity {
    let provides = vec![func_bound(kernel, name, arity)];
    kernel
        .add_item(ItemSpec {
            provides,
            requires,
            source: Some(format!("{name}(..) = ...")),
            ..Default::default()
        })
        .expect("add_item は失敗しない")
        .0
}

/// 全体を、常に同じ指紋で評価済みにする。
pub fn evaluate_all(kernel: &mut Kernel, digest_seed: u8) -> Vec<Entity> {
    let mut order = Vec::new();
    loop {
        let batch = kernel.next_batch();
        if batch.is_empty() {
            break;
        }
        let mut progressed = false;
        for entity in batch {
            let digest = digest_for(entity, digest_seed);
            if kernel
                .commit_evaluation(entity, EvalOutcome::Value { digest })
                .is_ok()
            {
                order.push(entity);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    order
}

/// エンティティごとに決まる指紋。`seed` を変えると全部が変わる。
pub fn digest_for(entity: Entity, seed: u8) -> [u8; 32] {
    let mut digest = [0u8; 32];
    digest[0] = seed;
    digest[1..5].copy_from_slice(&entity.index().to_le_bytes());
    digest
}

// ---------------------------------------------------------------------
// オラクル（素朴な参照実装）
// ---------------------------------------------------------------------

/// テスト用の Capability。整数 1 つで表す。
pub type OracleCapability = u16;

/// 1 つの項目。
#[derive(Clone, Debug, Default)]
pub struct OracleItem {
    pub provides: Vec<OracleCapability>,
    pub requires: Vec<OracleCapability>,
}

/// 効率を一切考えない参照実装。
///
/// 変更のたびに provider 索引をゼロから構築し、変更点からの到達可能集合を
/// すべて古いものとみなす。カーネルの増分保守が、この素朴な答えと一致する
/// ことを性質テスト P1 で確認する。
#[derive(Clone, Debug, Default)]
pub struct OracleKernel {
    pub items: Vec<Option<OracleItem>>,
}

impl OracleKernel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, item: OracleItem) -> usize {
        self.items.push(Some(item));
        self.items.len() - 1
    }

    pub fn remove(&mut self, index: usize) {
        if let Some(slot) = self.items.get_mut(index) {
            *slot = None;
        }
    }

    pub fn set(&mut self, index: usize, item: OracleItem) {
        if let Some(slot) = self.items.get_mut(index) {
            *slot = Some(item);
        }
    }

    pub fn alive(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.as_ref().map(|_| index))
            .collect()
    }

    /// 毎回ゼロから構築する provider 索引。
    fn provider_index(&self) -> HashMap<OracleCapability, Vec<usize>> {
        let mut index: HashMap<OracleCapability, Vec<usize>> = HashMap::new();
        for (position, slot) in self.items.iter().enumerate() {
            if let Some(item) = slot {
                for capability in &item.provides {
                    index.entry(*capability).or_default().push(position);
                }
            }
        }
        index
    }

    /// 上流（依存先）。
    pub fn upstream(&self, index: usize) -> BTreeSet<usize> {
        let providers = self.provider_index();
        let mut result = BTreeSet::new();
        if let Some(Some(item)) = self.items.get(index) {
            for capability in &item.requires {
                if let Some(sources) = providers.get(capability) {
                    for source in sources {
                        result.insert(*source);
                    }
                }
            }
        }
        result
    }

    /// 下流（依存元）。
    pub fn downstream(&self, index: usize) -> BTreeSet<usize> {
        self.alive()
            .into_iter()
            .filter(|candidate| self.upstream(*candidate).contains(&index))
            .collect()
    }

    /// 変更点から到達できるすべて（自分自身は含まない）。
    ///
    /// これが「原理的に古くなり得るもの」の完全な集合である。
    pub fn stale_after_change(&self, changed: usize) -> BTreeSet<usize> {
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mut queue: VecDeque<usize> = self.downstream(changed).into_iter().collect();
        while let Some(current) = queue.pop_front() {
            if current == changed || !seen.insert(current) {
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

    /// provider が存在しない requires を持つか。
    pub fn is_unresolved(&self, index: usize) -> bool {
        let providers = self.provider_index();
        match self.items.get(index) {
            Some(Some(item)) => item
                .requires
                .iter()
                .any(|capability| !providers.contains_key(capability)),
            _ => false,
        }
    }

    /// 上流への到達可能集合（自分自身は、循環していれば含まれる）。
    pub fn transitive_upstream(&self, index: usize) -> BTreeSet<usize> {
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mut queue: VecDeque<usize> = self.upstream(index).into_iter().collect();
        while let Some(current) = queue.pop_front() {
            if !seen.insert(current) {
                continue;
            }
            for next in self.upstream(current) {
                if !seen.contains(&next) {
                    queue.push_back(next);
                }
            }
        }
        seen
    }

    /// 原理的に `Clean` に到達できるか。
    ///
    /// カーネルは、循環に含まれるもの・未解決のもの・そして**上流にそれらを
    /// 持つもの**をスケジュールしない（`Kernel::next_batch` は全上流が `Clean`
    /// であることを要求する）。この関数はその条件を素朴に判定する。性質 P2 の
    /// 期待値になる。
    pub fn is_evaluable(&self, index: usize) -> bool {
        if self.items.get(index).map(Option::is_none).unwrap_or(true) {
            return false;
        }
        let mut targets = self.transitive_upstream(index);
        targets.insert(index);
        targets
            .into_iter()
            .all(|target| !self.is_in_cycle(target) && !self.is_unresolved(target))
    }

    /// 循環に含まれるか。素朴に、自分から自分へ戻れるかで判定する。
    pub fn is_in_cycle(&self, index: usize) -> bool {
        let mut seen: HashSet<usize> = HashSet::new();
        let mut queue: VecDeque<usize> = self.upstream(index).into_iter().collect();
        while let Some(current) = queue.pop_front() {
            if current == index {
                return true;
            }
            if !seen.insert(current) {
                continue;
            }
            for next in self.upstream(current) {
                queue.push_back(next);
            }
        }
        false
    }
}

/// カーネル側の操作を、同じ内容でオラクルにも適用するためのモデル。
pub struct Model {
    pub kernel: Kernel,
    pub oracle: OracleKernel,
    pub entities: Vec<Option<Entity>>,
    capabilities: Vec<Capability>,
}

impl Model {
    pub fn new(capability_count: u16) -> Self {
        let mut kernel = Kernel::new();
        let capabilities = (0..capability_count)
            .map(|index| Capability::NameBound(kernel.intern(&format!("c{index}"))))
            .collect();
        Self {
            kernel,
            oracle: OracleKernel::new(),
            entities: Vec::new(),
            capabilities,
        }
    }

    fn to_capabilities(&self, raw: &[OracleCapability]) -> Vec<Capability> {
        raw.iter()
            .filter_map(|index| self.capabilities.get(*index as usize).copied())
            .collect()
    }

    pub fn add(&mut self, item: OracleItem) -> usize {
        let spec = ItemSpec {
            provides: self.to_capabilities(&item.provides),
            requires: self.to_capabilities(&item.requires),
            ..Default::default()
        };
        let (entity, _) = self.kernel.add_item(spec).expect("add_item");
        let index = self.oracle.add(item);
        self.entities.push(Some(entity));
        index
    }

    pub fn change(&mut self, index: usize, item: OracleItem) -> KernelResult<()> {
        let entity = match self.entities.get(index).copied().flatten() {
            Some(entity) => entity,
            None => return Ok(()),
        };
        let update = ValueUpdate {
            provides: Some(self.to_capabilities(&item.provides)),
            requires: Some(self.to_capabilities(&item.requires)),
            ..Default::default()
        };
        self.kernel.change_value(entity, update)?;
        self.oracle.set(index, item);
        Ok(())
    }

    /// 関係を変えずに値だけ触る。
    pub fn touch(&mut self, index: usize) -> KernelResult<()> {
        let entity = match self.entities.get(index).copied().flatten() {
            Some(entity) => entity,
            None => return Ok(()),
        };
        self.kernel.change_value(entity, ValueUpdate::default())?;
        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> KernelResult<()> {
        let entity = match self.entities.get(index).copied().flatten() {
            Some(entity) => entity,
            None => return Ok(()),
        };
        self.kernel.remove_item(entity)?;
        self.oracle.remove(index);
        self.entities[index] = None;
        Ok(())
    }

    /// 早期カットオフの有効・無効。
    pub fn set_early_cutoff(&mut self, enabled: bool) {
        self.kernel.set_early_cutoff(enabled);
    }

    /// 収束するまで評価する。`seed` を変えると全エンティティの指紋が変わる。
    pub fn evaluate(&mut self, seed: u8) -> Vec<Entity> {
        evaluate_all(&mut self.kernel, seed)
    }

    /// 収束するまで評価し、評価されたものを添字で返す。
    pub fn evaluate_indices(&mut self, seed: u8) -> Vec<usize> {
        self.evaluate(seed)
            .into_iter()
            .filter_map(|entity| self.index_of(entity))
            .collect()
    }

    pub fn entity(&self, index: usize) -> Option<Entity> {
        self.entities.get(index).copied().flatten()
    }

    pub fn index_of(&self, entity: Entity) -> Option<usize> {
        self.entities.iter().position(|slot| *slot == Some(entity))
    }

    /// 現在 `Clean` な項目の添字。
    pub fn clean_indices(&self) -> BTreeSet<usize> {
        self.entities
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let entity = (*slot)?;
                match self.kernel.freshness(entity) {
                    Ok(mathrel_kernel::Freshness::Clean { .. }) => Some(index),
                    _ => None,
                }
            })
            .collect()
    }

    /// 現在古い（Dirty または MaybeDirty）項目の添字。
    pub fn stale_indices(&self) -> BTreeSet<usize> {
        self.kernel
            .stale_set()
            .into_iter()
            .filter_map(|entity| self.index_of(entity))
            .collect()
    }
}
