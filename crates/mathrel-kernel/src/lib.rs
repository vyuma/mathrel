//! # mathrel-kernel
//!
//! Relational Math Kernel のコア。数式・定義・宣言を、相互に依存する数学
//! オブジェクトとして管理する。
//!
//! ## このカーネルがすること
//!
//! 1. 誰が誰に依存しているかを追跡する
//! 2. いま何が古いかを追跡する
//!
//! ## このカーネルがしないこと
//!
//! 評価しない。パースしない。数式の意味を解釈しない。I/O をしない。
//! 計算は上位層（ホスト）が行い、結果の指紋を報告する。
//!
//! この線引きにより、カーネルは純粋・同期・I/O なしになり、素朴実装との
//! 等価性を性質テストで確認できる。
//!
//! ## 依存の登録は明示的でなければならない
//!
//! カーネルが [`Expr`] の中身をパースして依存を推測することは禁止している。
//! `requires` は必ずホストが登録する。解析で依存を推測する系は、解析の限界が
//! そのまま健全性の穴になるからである（arXiv:2511.21994 が測定した IPyflow
//! 型の失敗モード）。
//!
//! ## 例
//!
//! ```
//! use mathrel_kernel::{Capability, EvalOutcome, ItemSpec, Kernel, ValueUpdate};
//!
//! let mut kernel = Kernel::new();
//! let x = kernel.intern("x");
//! let y = kernel.intern("y");
//!
//! // x = 2
//! let (x_entity, _) = kernel
//!     .add_item(ItemSpec {
//!         provides: vec![Capability::NameBound(x)],
//!         ..Default::default()
//!     })
//!     .expect("add x");
//!
//! // y = x + 1  （requires はホストが明示的に登録する）
//! let (y_entity, _) = kernel
//!     .add_item(ItemSpec {
//!         provides: vec![Capability::NameBound(y)],
//!         requires: vec![Capability::NameBound(x)],
//!         ..Default::default()
//!     })
//!     .expect("add y");
//!
//! assert_eq!(kernel.dependencies(y_entity).expect("deps"), vec![x_entity]);
//!
//! // 両方を評価して Clean にする
//! kernel.run_to_fixpoint(|_, _| EvalOutcome::Value { digest: [0; 32] });
//!
//! // x を変えると y が古くなる
//! let report = kernel
//!     .change_value(x_entity, ValueUpdate::default())
//!     .expect("change");
//! assert!(report.newly_dirty.contains(&y_entity));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod component;
pub mod entity;
pub mod error;
pub mod graph;
pub mod kernel;
pub mod relation;
pub mod state;
pub mod symbol;

pub use component::{ComponentStore, Decl, DeclKind, DisplayHint, Expr, MapStore, Source, TypeInfo};
pub use entity::{Entity, EntityAllocator};
pub use error::{KernelError, KernelResult};
pub use graph::DependencyGraph;
pub use kernel::{ChangeReport, EvalOutcome, ItemSpec, Kernel, ValueUpdate};
pub use relation::Capability;
pub use state::{Digest, EvalState, Freshness, Resolution, Revision};
pub use symbol::{Symbol, SymbolInterner};
