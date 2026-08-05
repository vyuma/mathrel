//! Entity に付随するデータ。
//!
//! カーネルが解釈するのは [`Decl`] だけである。[`Expr`] / [`Source`] /
//! [`DisplayHint`] は不透明であり、[`TypeInfo`] は等値比較しかしない。
//!
//! **重要**: カーネルが [`Expr`] の中身をパースして依存を推測することは
//! 禁止する。`requires` は必ずホストが明示的に登録する。解析で依存を
//! 推測する系は、解析の限界がそのまま健全性の穴になる
//! （IPyflow 型の失敗モード, arXiv:2511.21994）。

use crate::entity::Entity;
use crate::symbol::Symbol;
use rustc_hash::FxHashMap;

/// 宣言の種別。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeclKind {
    /// 値の束縛。
    Value,
    /// 関数の束縛。
    Function {
        /// 引数の個数。
        arity: u8,
    },
    /// 名前を提供しない式（単なる評価要求など）。
    Anonymous,
}

/// 宣言。カーネルが解釈する唯一のコンポーネント。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decl {
    /// 宣言される名前。`Anonymous` の場合も持ち得る（表示用）。
    pub name: Symbol,
    /// 宣言の種別。
    pub kind: DeclKind,
}

/// ホストが供給する中間表現。カーネルにとっては不透明なバイト列である。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Expr(pub Vec<u8>);

impl Expr {
    /// 文字列から作る。ホストが何を入れるかはホストの自由である。
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Expr(text.as_bytes().to_vec())
    }
}

/// 型情報。カーネルは等値比較しかしない。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TypeInfo {
    /// 型を表す不透明トークン。未確定なら `None`。
    pub type_token: Option<String>,
}

/// 表示ヒント。カーネルにとって不透明。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DisplayHint(pub String);

/// ユーザ入力の原文字列。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Source(pub String);

/// コンポーネントの格納庫。
///
/// 実装を差し替えられるようトレイトにしてある（要件定義書 ADR-002）。
/// v0.1 の具体実装はハッシュマップだが、後日 ECS の archetype storage に
/// 移せるようにしておく。
pub trait ComponentStore<T> {
    /// 値を引く。
    fn get(&self, entity: Entity) -> Option<&T>;
    /// 値を設定する。
    fn set(&mut self, entity: Entity, value: T);
    /// 値を取り除く。
    fn remove(&mut self, entity: Entity) -> Option<T>;
    /// 全件を走査する。順序は保証しない。
    fn iter<'a>(&'a self) -> impl Iterator<Item = (Entity, &'a T)>
    where
        T: 'a;
}

/// ハッシュマップによる素朴な [`ComponentStore`] 実装。
#[derive(Debug, Clone)]
pub struct MapStore<T> {
    inner: FxHashMap<Entity, T>,
}

impl<T> Default for MapStore<T> {
    fn default() -> Self {
        Self {
            inner: FxHashMap::default(),
        }
    }
}

impl<T> MapStore<T> {
    /// 空のストアを作る。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 格納数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// 空か。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// 可変参照を引く。
    pub fn get_mut(&mut self, entity: Entity) -> Option<&mut T> {
        self.inner.get_mut(&entity)
    }
}

impl<T> ComponentStore<T> for MapStore<T> {
    fn get(&self, entity: Entity) -> Option<&T> {
        self.inner.get(&entity)
    }

    fn set(&mut self, entity: Entity, value: T) {
        self.inner.insert(entity, value);
    }

    fn remove(&mut self, entity: Entity) -> Option<T> {
        self.inner.remove(&entity)
    }

    fn iter<'a>(&'a self) -> impl Iterator<Item = (Entity, &'a T)>
    where
        T: 'a,
    {
        self.inner.iter().map(|(entity, value)| (*entity, value))
    }
}
