//! # mathrel-host
//!
//! [`mathrel_kernel`] のホスト層。カーネルがしないことを引き受ける。
//!
//! - LaTeX 風入力のパース（[`parser`]）
//! - `requires` / `provides` の抽出（[`extract`]）
//! - 評価（[`eval`]）
//! - セル管理とカーネルの駆動（[`workspace`]）
//!
//! カーネルとの責務分担は企画書 §6.1 の通り。カーネルは「誰が誰に依存して
//! いるか」と「いま何が古いか」だけを知り、評価は必ずこの層が行う。
//!
//! ```
//! use mathrel_host::Workspace;
//!
//! let mut workspace = Workspace::new();
//! let x = workspace.add_cell("x = 2");
//! workspace.add_cell("f(t) = t^2 + 1");
//! let y = workspace.add_cell("y = f(x)");
//! workspace.evaluate();
//!
//! assert_eq!(workspace.value(y).map(|v| v.render()), Some("5".to_owned()));
//!
//! // x を変えると y だけが再計算される
//! workspace.update_cell(x, "x = 3").expect("update");
//! workspace.evaluate();
//! assert_eq!(workspace.value(y).map(|v| v.render()), Some("10".to_owned()));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod ast;
pub mod eval;
pub mod extract;
pub mod json;
pub mod parser;
pub mod value;
pub mod workspace;

pub use ast::{Ast, BinOp, Stmt};
pub use eval::{eval, Env, EvalError, FuncDefinition};
pub use extract::{extract, CapabilitySpec, RelationSpec};
pub use parser::{parse_statement, ParseError};
pub use value::{digest_of, digest_of_value, Value};
pub use workspace::{
    parse_obligation_line, Cell, CellId, CellKind, EvalStats, WeakLink, Workspace, WorkspaceError,
};
