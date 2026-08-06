//! `requires` / `provides` の抽出。
//!
//! ここがホスト層の責務の中心である。カーネルは式の中身を見ないと決めた
//! （企画書 §6.2）ので、依存の抽出は必ずここを通る。解析の限界がそのまま
//! 健全性の穴になるのは、この関数の中だけに閉じている。
//!
//! ## 規則
//!
//! | 文 | provides | requires |
//! |---|---|---|
//! | `x = e` | `NameBound(x)` | `e` の自由な名前 |
//! | `f(p) = e` | `FunctionBound(f/1)` | `e` の自由な名前（`p` を除く） |
//! | `v : T` | `TypeKnown(v)` | なし |
//! | `e` | なし | `e` の自由な名前 |
//!
//! `norm(v)` のように意味が型に依存する呼び出しは、`NameBound(v)` に加えて
//! `TypeKnown(v)` も要求する。これにより、型宣言がないうちは未解決のまま
//! 保持され、「`v` の型を指定してください」と言える（企画書 §2）。

use crate::ast::{Ast, Stmt};
use crate::eval::{builtin_constant, builtin_function_arity};
use std::collections::BTreeSet;

/// Capability の名前ベースの記述。
///
/// カーネルの `Capability` は [`mathrel_kernel::Symbol`] を持つため、
/// インターナを持たないこの層では文字列で表す。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum CapabilitySpec {
    /// 名前が値として束縛されている。
    NameBound(String),
    /// 名前がアリティ `arity` の関数として束縛されている。
    FunctionBound {
        /// 関数名。
        name: String,
        /// 引数の個数。
        arity: u8,
    },
    /// 名前の型が確定している。
    TypeKnown(String),
}

/// 抽出結果。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RelationSpec {
    /// 提供する Capability。
    pub provides: Vec<CapabilitySpec>,
    /// 要求する Capability。
    pub requires: Vec<CapabilitySpec>,
}

/// 文から `requires` / `provides` を抽出する。
///
/// ```
/// use mathrel_host::extract::{extract, CapabilitySpec};
/// use mathrel_host::parser::parse_statement;
///
/// let stmt = parse_statement("y = f(x)").expect("parse");
/// let relations = extract(&stmt);
/// assert_eq!(
///     relations.provides,
///     vec![CapabilitySpec::NameBound("y".to_owned())]
/// );
/// assert!(relations
///     .requires
///     .contains(&CapabilitySpec::NameBound("x".to_owned())));
/// assert!(relations.requires.contains(&CapabilitySpec::FunctionBound {
///     name: "f".to_owned(),
///     arity: 1
/// }));
/// ```
#[must_use]
pub fn extract(stmt: &Stmt) -> RelationSpec {
    let mut provides = Vec::new();
    let mut requires = BTreeSet::new();
    let mut bound = BTreeSet::new();

    match stmt {
        Stmt::ValueDef { name, body } => {
            provides.push(CapabilitySpec::NameBound(name.clone()));
            collect(body, &bound, &mut requires);
        }
        Stmt::FuncDef { name, params, body } => {
            let arity = u8::try_from(params.len()).unwrap_or(u8::MAX);
            provides.push(CapabilitySpec::FunctionBound {
                name: name.clone(),
                arity,
            });
            for param in params {
                bound.insert(param.clone());
            }
            collect(body, &bound, &mut requires);
        }
        Stmt::TypeDecl { name, .. } => {
            provides.push(CapabilitySpec::TypeKnown(name.clone()));
        }
        Stmt::Anonymous { body } => {
            collect(body, &bound, &mut requires);
        }
    }

    RelationSpec {
        provides,
        requires: requires.into_iter().collect(),
    }
}

/// 意味が引数の型に依存する組み込み関数。
///
/// これらを裸の名前に適用した場合、`TypeKnown` も要求する。
const TYPE_SENSITIVE: &[&str] = &["norm"];

fn collect(ast: &Ast, bound: &BTreeSet<String>, out: &mut BTreeSet<CapabilitySpec>) {
    match ast {
        Ast::Number(_) => {}
        Ast::Variable(name) => {
            if !bound.contains(name) && builtin_constant(name).is_none() {
                out.insert(CapabilitySpec::NameBound(name.clone()));
            }
        }
        Ast::VectorLit(items) => {
            for item in items {
                collect(item, bound, out);
            }
        }
        Ast::Negate(inner) => collect(inner, bound, out),
        Ast::Binary(_, left, right) => {
            collect(left, bound, out);
            collect(right, bound, out);
        }
        Ast::Call(name, args) => {
            if builtin_function_arity(name).is_none() {
                let arity = u8::try_from(args.len()).unwrap_or(u8::MAX);
                out.insert(CapabilitySpec::FunctionBound {
                    name: name.clone(),
                    arity,
                });
            } else if TYPE_SENSITIVE.contains(&name.as_str()) {
                if let Some(Ast::Variable(variable)) = args.first() {
                    if !bound.contains(variable) {
                        out.insert(CapabilitySpec::TypeKnown(variable.clone()));
                    }
                }
            }
            for arg in args {
                collect(arg, bound, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_statement;

    fn relations(text: &str) -> RelationSpec {
        extract(&parse_statement(text).expect("parse"))
    }

    #[test]
    fn value_definition_provides_a_name() {
        let spec = relations("x = 2");
        assert_eq!(spec.provides, vec![CapabilitySpec::NameBound("x".into())]);
        assert!(spec.requires.is_empty());
    }

    #[test]
    fn function_parameters_are_not_required() {
        let spec = relations("f(t) = t^2 + 1");
        assert_eq!(
            spec.provides,
            vec![CapabilitySpec::FunctionBound {
                name: "f".into(),
                arity: 1
            }]
        );
        assert!(spec.requires.is_empty(), "t は仮引数なので依存にしない");
    }

    #[test]
    fn free_variables_in_a_function_body_are_required() {
        let spec = relations("f(t) = t^2 + c");
        assert_eq!(spec.requires, vec![CapabilitySpec::NameBound("c".into())]);
    }

    #[test]
    fn builtins_do_not_create_dependencies() {
        let spec = relations("y = sqrt(2) + pi");
        assert!(spec.requires.is_empty());
    }

    #[test]
    fn value_and_function_of_the_same_name_are_distinct_capabilities() {
        let value = relations("f = 3");
        let function = relations("f(t) = t");
        assert_ne!(value.provides, function.provides);
    }

    #[test]
    fn norm_of_a_bare_name_also_requires_its_type() {
        let spec = relations("n = norm(v)");
        assert!(spec
            .requires
            .contains(&CapabilitySpec::NameBound("v".into())));
        assert!(spec
            .requires
            .contains(&CapabilitySpec::TypeKnown("v".into())));
    }

    #[test]
    fn norm_of_a_literal_needs_no_type() {
        let spec = relations("n = norm([3, 4])");
        assert!(spec.requires.is_empty());
    }

    #[test]
    fn type_declaration_provides_type_known() {
        let spec = relations("v : Vector[Real]");
        assert_eq!(spec.provides, vec![CapabilitySpec::TypeKnown("v".into())]);
    }

    #[test]
    fn anonymous_expression_provides_nothing() {
        let spec = relations("f(x) + 1");
        assert!(spec.provides.is_empty());
        assert_eq!(spec.requires.len(), 2);
    }
}
