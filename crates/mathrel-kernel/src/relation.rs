//! Capability — 「提供され得るもの」の識別子。
//!
//! `requires` / `provides` は Scope Graph（Néron et al., ESOP 2015）における
//! 参照 / 宣言と同型である。参照が解決されないことをエラーにせず、
//! 「解決パスの不在」という第一級の状態として扱う点も同様。

use crate::symbol::Symbol;

/// エンティティが要求／提供する能力。
///
/// この列挙は将来拡張される。`match` の網羅性に強く依存する構造を書かないこと。
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
#[non_exhaustive]
pub enum Capability {
    /// 名前が値として束縛されている。
    NameBound(Symbol),
    /// 名前がアリティ `arity` の関数として束縛されている。
    ///
    /// `f = 3`（`NameBound`）と `f(t) = t^2`（`FunctionBound`）は
    /// 別の Capability である。両者が同居しても曖昧にはならない。
    FunctionBound {
        /// 関数名。
        name: Symbol,
        /// 引数の個数。
        arity: u8,
    },
    /// 名前の型が確定している。
    TypeKnown(Symbol),
    /// 命題が証明済みである。
    ///
    /// 検証義務（`mathrel-verify`）が使う。`定理 B` が `補題 A` を引用するなら
    /// `B.requires = [PropositionProved(A)]` と書く。補題を直せば定理が
    /// `Dirty` になり、再検査の対象になる。
    ///
    /// `NameBound` と分けているのは、`A` という名前の値と `A` という名前の
    /// 命題が同居できるようにするためである（`NameBound` と `FunctionBound`
    /// を分けたのと同じ理由）。
    PropositionProved(Symbol),
}

impl Capability {
    /// 対象となる名前。
    #[must_use]
    pub fn name(self) -> Symbol {
        match self {
            Capability::NameBound(name)
            | Capability::FunctionBound { name, .. }
            | Capability::TypeKnown(name)
            | Capability::PropositionProved(name) => name,
        }
    }

    /// 種別を表す短い文字列。表示とスナップショット用。
    #[must_use]
    pub fn kind_str(self) -> &'static str {
        match self {
            Capability::NameBound(_) => "NameBound",
            Capability::FunctionBound { .. } => "FunctionBound",
            Capability::TypeKnown(_) => "TypeKnown",
            Capability::PropositionProved(_) => "PropositionProved",
        }
    }
}
