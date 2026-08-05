//! 文字列のインターン。
//!
//! Capability の比較はカーネルの内側で頻繁に起きるため、文字列比較ではなく
//! 整数比較になるようにインターンする。

use indexmap::IndexSet;

/// インターン済み文字列への参照。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Symbol(u32);

impl Symbol {
    /// 内部 ID。表示順序の安定化にのみ使う。
    #[must_use]
    pub fn id(self) -> u32 {
        self.0
    }
}

/// 文字列 ↔ [`Symbol`] の対応表。
#[derive(Default, Debug, Clone)]
pub struct SymbolInterner {
    entries: IndexSet<String>,
}

impl SymbolInterner {
    /// 空のインターナを作る。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 文字列を登録し、[`Symbol`] を返す。既存なら同じ Symbol を返す。
    pub fn intern(&mut self, text: &str) -> Symbol {
        if let Some(index) = self.entries.get_index_of(text) {
            return Symbol(index as u32);
        }
        let (index, _) = self.entries.insert_full(text.to_owned());
        Symbol(index as u32)
    }

    /// 登録済みの文字列を引く。
    #[must_use]
    pub fn resolve(&self, symbol: Symbol) -> Option<&str> {
        self.entries.get_index(symbol.0 as usize).map(String::as_str)
    }

    /// 登録済みの文字列を引く。未登録なら `"<unknown>"`。
    ///
    /// 表示用。`resolve` が None を返すのは Symbol を別のインターナから
    /// 持ち込んだ場合だけであり、通常は起きない。
    #[must_use]
    pub fn name(&self, symbol: Symbol) -> &str {
        self.resolve(symbol).unwrap_or("<unknown>")
    }

    /// 登録数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空か。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_idempotent() {
        let mut interner = SymbolInterner::new();
        let a = interner.intern("x");
        let b = interner.intern("x");
        let c = interner.intern("y");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(interner.resolve(a), Some("x"));
        assert_eq!(interner.resolve(c), Some("y"));
        assert_eq!(interner.len(), 2);
    }
}
