//! カーネルのエラー型。
//!
//! カーネルはパニックしない。不正な入力はすべて [`KernelError`] になる。

use crate::entity::Entity;
use thiserror::Error;

/// カーネル操作の失敗。
#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KernelError {
    /// 削除済みの Entity ハンドルが渡された。
    #[error("entity {0} has been removed")]
    StaleEntity(Entity),
    /// 存在しない Entity ハンドルが渡された。
    #[error("entity {0} does not exist")]
    UnknownEntity(Entity),
    /// 循環に含まれるエンティティに対する評価報告。
    #[error("cannot commit evaluation for entity in cycle: {0}")]
    CommitOnCyclic(Entity),
    /// 未解決のエンティティに対する評価報告。
    #[error("cannot commit evaluation for unresolved entity: {0}")]
    CommitOnUnresolved(Entity),
    /// 既に Clean なエンティティに対する評価報告。
    #[error("entity {0} is not dirty; commit rejected")]
    CommitOnClean(Entity),
}

/// カーネル操作の結果。
pub type KernelResult<T> = Result<T, KernelError>;
