//! Entity の identity と世代管理。
//!
//! Entity は世代付きインデックスである。削除された index は再利用されるが、
//! その際 generation が +1 されるため、古いハンドルでのアクセスは
//! [`crate::KernelError::StaleEntity`] として検出される。パニックはしない。

/// 数学オブジェクトの識別子。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Entity {
    index: u32,
    generation: u32,
}

impl Entity {
    /// 内部インデックス。安定した表示順序を作るためだけに公開している。
    #[must_use]
    pub fn index(self) -> u32 {
        self.index
    }

    /// 世代番号。
    #[must_use]
    pub fn generation(self) -> u32 {
        self.generation
    }
}

impl core::fmt::Display for Entity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "e{}v{}", self.index, self.generation)
    }
}

/// 生存状態を持つスロット。
#[derive(Clone, Copy, Debug)]
struct Slot {
    generation: u32,
    alive: bool,
}

/// Entity の割り当てと世代管理。
#[derive(Default, Debug, Clone)]
pub struct EntityAllocator {
    slots: Vec<Slot>,
    free: Vec<u32>,
}

impl EntityAllocator {
    /// 空のアロケータを作る。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 新しい Entity を確保する。
    pub fn allocate(&mut self) -> Entity {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.alive = true;
            Entity {
                index,
                generation: slot.generation,
            }
        } else {
            let index = u32::try_from(self.slots.len()).unwrap_or(u32::MAX);
            self.slots.push(Slot {
                generation: 0,
                alive: true,
            });
            Entity {
                index,
                generation: 0,
            }
        }
    }

    /// Entity を解放する。以後、同じハンドルは stale になる。
    ///
    /// 解放に成功したときだけ `true` を返す。
    pub fn deallocate(&mut self, entity: Entity) -> bool {
        match self.slots.get_mut(entity.index as usize) {
            Some(slot) if slot.alive && slot.generation == entity.generation => {
                slot.alive = false;
                slot.generation = slot.generation.wrapping_add(1);
                self.free.push(entity.index);
                true
            }
            _ => false,
        }
    }

    /// Entity が現在生きているか。
    #[must_use]
    pub fn is_alive(&self, entity: Entity) -> bool {
        matches!(
            self.slots.get(entity.index as usize),
            Some(slot) if slot.alive && slot.generation == entity.generation
        )
    }

    /// index が存在するが世代が古い場合に true。
    #[must_use]
    pub fn is_stale(&self, entity: Entity) -> bool {
        match self.slots.get(entity.index as usize) {
            Some(slot) => slot.generation != entity.generation || !slot.alive,
            None => false,
        }
    }

    /// 生存中の Entity を index 昇順で列挙する。
    pub fn iter_alive(&self) -> impl Iterator<Item = Entity> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.alive)
            .map(|(index, slot)| Entity {
                index: index as u32,
                generation: slot.generation,
            })
    }

    /// 生存中の Entity 数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|slot| slot.alive).count()
    }

    /// 生存中の Entity がないか。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_produces_distinct_entities() {
        let mut alloc = EntityAllocator::new();
        let a = alloc.allocate();
        let b = alloc.allocate();
        assert_ne!(a, b);
        assert!(alloc.is_alive(a));
        assert!(alloc.is_alive(b));
    }

    #[test]
    fn deallocated_handle_becomes_stale_and_index_is_reused() {
        let mut alloc = EntityAllocator::new();
        let a = alloc.allocate();
        assert!(alloc.deallocate(a));
        assert!(!alloc.is_alive(a));
        assert!(alloc.is_stale(a));

        let b = alloc.allocate();
        assert_eq!(a.index(), b.index(), "index は再利用される");
        assert_ne!(a.generation(), b.generation(), "世代は上がる");
        assert!(alloc.is_alive(b));
        assert!(!alloc.is_alive(a), "古いハンドルは無効なまま");
    }

    #[test]
    fn double_deallocate_is_rejected() {
        let mut alloc = EntityAllocator::new();
        let a = alloc.allocate();
        assert!(alloc.deallocate(a));
        assert!(!alloc.deallocate(a));
    }

    #[test]
    fn iter_alive_is_index_ordered() {
        let mut alloc = EntityAllocator::new();
        let a = alloc.allocate();
        let b = alloc.allocate();
        let c = alloc.allocate();
        alloc.deallocate(b);
        let seen: Vec<_> = alloc.iter_alive().collect();
        assert_eq!(seen, vec![a, c]);
    }
}
