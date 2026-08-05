//! 評価器。
//!
//! ホスト層の一部である。カーネルは評価しない（企画書 ADR-004）。
//! カーネルが `next_batch()` で返したエンティティを、ここで評価し、
//! 結果の指紋を `commit_evaluation()` で報告する。
//!
//! 環境（どの名前が何に束縛されているか）は**依存グラフから導出する**。
//! グローバルなシンボルテーブルを別に持たない。これにより、カーネルが
//! 追跡している依存と、実際に評価で使われる束縛が、構造的に一致する。

use crate::ast::{Ast, BinOp};
use crate::value::Value;
use std::collections::HashMap;

/// 評価失敗。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EvalError {
    /// 人間向けの説明。
    pub message: String,
}

impl EvalError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl core::fmt::Display for EvalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EvalError {}

/// 関数定義。
#[derive(Clone, PartialEq, Debug)]
pub struct FuncDefinition {
    /// 仮引数。
    pub params: Vec<String>,
    /// 本体。
    pub body: Ast,
}

/// 評価環境。上流エンティティから組み立てられる。
#[derive(Clone, Default, Debug)]
pub struct Env {
    /// 名前 → 値。
    pub values: HashMap<String, Value>,
    /// (名前, アリティ) → 関数定義。
    pub functions: HashMap<(String, usize), FuncDefinition>,
    /// 名前 → 宣言された型トークン。
    pub types: HashMap<String, String>,
}

impl Env {
    /// 空の環境。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// 再帰の上限。ホストもパニックしない。
const MAX_DEPTH: usize = 128;

/// 組み込み定数。これらは `requires` を生まない。
#[must_use]
pub fn builtin_constant(name: &str) -> Option<f64> {
    match name {
        "pi" => Some(std::f64::consts::PI),
        "tau" => Some(std::f64::consts::TAU),
        "e" => Some(std::f64::consts::E),
        "inf" => Some(f64::INFINITY),
        _ => None,
    }
}

/// 組み込み関数の名前とアリティ。これらは `requires` を生まない。
#[must_use]
pub fn builtin_function_arity(name: &str) -> Option<&'static [usize]> {
    const UNARY: &[usize] = &[1];
    const BINARY: &[usize] = &[2];
    match name {
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "exp" | "ln" | "sqrt" | "abs"
        | "floor" | "ceil" | "round" | "sign" | "norm" | "sum" | "length" => Some(UNARY),
        "log" | "min" | "max" | "dot" | "pow" | "atan2" => Some(BINARY),
        _ => None,
    }
}

/// 式を評価する。
pub fn eval(ast: &Ast, env: &Env) -> Result<Value, EvalError> {
    eval_with_depth(ast, env, 0)
}

fn eval_with_depth(ast: &Ast, env: &Env, depth: usize) -> Result<Value, EvalError> {
    if depth > MAX_DEPTH {
        return Err(EvalError::new(
            "再帰が深すぎます。循環した定義かもしれません",
        ));
    }
    match ast {
        Ast::Number(value) => Ok(Value::Scalar(*value)),
        Ast::Variable(name) => {
            if let Some(value) = env.values.get(name) {
                return Ok(value.clone());
            }
            if let Some(constant) = builtin_constant(name) {
                return Ok(Value::Scalar(constant));
            }
            Err(EvalError::new(format!("{name} が未定義です")))
        }
        Ast::VectorLit(items) => {
            let mut components = Vec::with_capacity(items.len());
            for item in items {
                match eval_with_depth(item, env, depth + 1)? {
                    Value::Scalar(scalar) => components.push(scalar),
                    Value::Vector(_) => {
                        return Err(EvalError::new("ベクトルの入れ子はまだ扱えません"))
                    }
                }
            }
            Ok(Value::Vector(components))
        }
        Ast::Negate(inner) => match eval_with_depth(inner, env, depth + 1)? {
            Value::Scalar(scalar) => Ok(Value::Scalar(-scalar)),
            Value::Vector(items) => Ok(Value::Vector(items.into_iter().map(|x| -x).collect())),
        },
        Ast::Binary(op, left, right) => {
            let left = eval_with_depth(left, env, depth + 1)?;
            let right = eval_with_depth(right, env, depth + 1)?;
            apply_binary(*op, &left, &right)
        }
        Ast::Call(name, args) => {
            let mut evaluated = Vec::with_capacity(args.len());
            for arg in args {
                evaluated.push(eval_with_depth(arg, env, depth + 1)?);
            }

            // 組み込みの名前は上書きできない。依存抽出（`extract`）が組み込みに
            // 対して `requires` を出さないため、上書きを許すと「評価では使うが
            // カーネルは依存として知らない」束縛が生まれ、健全性の穴になる。
            if builtin_function_arity(name).is_some() {
                return apply_builtin(name, &evaluated, args, env);
            }

            if let Some(definition) = env.functions.get(&(name.clone(), evaluated.len())) {
                let mut local = env.clone();
                for (param, value) in definition.params.iter().zip(evaluated.iter()) {
                    local.values.insert(param.clone(), value.clone());
                }
                return eval_with_depth(&definition.body, &local, depth + 1);
            }

            apply_builtin(name, &evaluated, args, env)
        }
    }
}

fn apply_binary(op: BinOp, left: &Value, right: &Value) -> Result<Value, EvalError> {
    use Value::{Scalar, Vector};
    match (op, left, right) {
        (_, Scalar(a), Scalar(b)) => Ok(Scalar(scalar_op(op, *a, *b))),

        (BinOp::Add | BinOp::Sub, Vector(a), Vector(b)) => {
            if a.len() != b.len() {
                return Err(EvalError::new(format!(
                    "長さの違うベクトルは足せません: {} と {}",
                    a.len(),
                    b.len()
                )));
            }
            Ok(Vector(
                a.iter()
                    .zip(b.iter())
                    .map(|(x, y)| scalar_op(op, *x, *y))
                    .collect(),
            ))
        }
        (BinOp::Mul, Scalar(a), Vector(b)) => Ok(Vector(b.iter().map(|y| a * y).collect())),
        (BinOp::Mul | BinOp::Div, Vector(a), Scalar(b)) => {
            Ok(Vector(a.iter().map(|x| scalar_op(op, *x, *b)).collect()))
        }
        (BinOp::Mul, Vector(_), Vector(_)) => Err(EvalError::new(
            "ベクトル同士の積は曖昧です。dot(u, v) と書いてください",
        )),
        _ => Err(EvalError::new(format!(
            "{} を {} と {} には適用できません",
            op.symbol(),
            left.type_name(),
            right.type_name()
        ))),
    }
}

fn scalar_op(op: BinOp, a: f64, b: f64) -> f64 {
    match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        BinOp::Pow => a.powf(b),
    }
}

/// 組み込み関数を適用する。
///
/// `norm` はここで**意味の解決**を行う。引数が裸の名前なら、宣言された型を
/// 見る。型が宣言されていなければ、値の実際の形に落とす。企画書 §2 の
/// 「`v` の型を `Real` に変えると `\norm{v}` の意味が `Abs(v)` に変わる」を
/// 実装したものである。
fn apply_builtin(
    name: &str,
    args: &[Value],
    raw_args: &[Ast],
    env: &Env,
) -> Result<Value, EvalError> {
    let arity_error = |expected: usize| {
        EvalError::new(format!(
            "{name} は引数 {expected} 個です（{} 個渡されました）",
            args.len()
        ))
    };

    match (name, args) {
        ("norm", [value]) => {
            let declared = match raw_args.first() {
                Some(Ast::Variable(variable)) => env.types.get(variable).map(String::as_str),
                _ => None,
            };
            resolve_norm(declared, value)
        }
        ("abs", [Value::Scalar(x)]) => Ok(Value::Scalar(x.abs())),
        ("abs", [Value::Vector(items)]) => Ok(Value::Scalar(euclidean_norm(items))),
        ("sum", [Value::Vector(items)]) => Ok(Value::Scalar(items.iter().sum())),
        ("sum", [Value::Scalar(x)]) => Ok(Value::Scalar(*x)),
        ("length", [Value::Vector(items)]) => Ok(Value::Scalar(items.len() as f64)),
        ("length", [Value::Scalar(_)]) => Ok(Value::Scalar(1.0)),
        ("dot", [Value::Vector(a), Value::Vector(b)]) => {
            if a.len() != b.len() {
                return Err(EvalError::new("dot は同じ長さのベクトルにだけ使えます"));
            }
            Ok(Value::Scalar(a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()))
        }

        (_, [Value::Scalar(x)]) => {
            let result = match name {
                "sin" => x.sin(),
                "cos" => x.cos(),
                "tan" => x.tan(),
                "asin" => x.asin(),
                "acos" => x.acos(),
                "atan" => x.atan(),
                "exp" => x.exp(),
                "ln" => x.ln(),
                "sqrt" => x.sqrt(),
                "floor" => x.floor(),
                "ceil" => x.ceil(),
                "round" => x.round(),
                "sign" => x.signum(),
                _ => {
                    return Err(match builtin_function_arity(name) {
                        Some(arities) => arity_error(arities[0]),
                        None => EvalError::new(format!("{name} は未定義の関数です")),
                    })
                }
            };
            Ok(Value::Scalar(result))
        }
        (_, [Value::Scalar(a), Value::Scalar(b)]) => {
            let result = match name {
                "log" => a.log(*b),
                "min" => a.min(*b),
                "max" => a.max(*b),
                "pow" => a.powf(*b),
                "atan2" => a.atan2(*b),
                _ => {
                    return Err(match builtin_function_arity(name) {
                        Some(arities) => arity_error(arities[0]),
                        None => EvalError::new(format!("{name} は未定義の関数です")),
                    })
                }
            };
            Ok(Value::Scalar(result))
        }
        _ => Err(match builtin_function_arity(name) {
            Some(arities) => arity_error(arities[0]),
            None => EvalError::new(format!("{name} は未定義の関数です")),
        }),
    }
}

/// `norm` の意味を、宣言された型に応じて決める。
fn resolve_norm(declared_type: Option<&str>, value: &Value) -> Result<Value, EvalError> {
    let head = declared_type.map(type_head);
    match (head.as_deref(), value) {
        // 型が Real と宣言されているなら Abs。
        (Some("Real") | Some("Integer") | Some("Natural") | Some("Scalar"), Value::Scalar(x)) => {
            Ok(Value::Scalar(x.abs()))
        }
        (Some("Real") | Some("Integer") | Some("Natural") | Some("Scalar"), Value::Vector(_)) => {
            Err(EvalError::new(
                "型は Real と宣言されていますが、値はベクトルです",
            ))
        }
        // 型が Vector と宣言されているならユークリッドノルム。
        (Some("Vector"), Value::Vector(items)) => Ok(Value::Scalar(euclidean_norm(items))),
        (Some("Vector"), Value::Scalar(_)) => Err(EvalError::new(
            "型は Vector と宣言されていますが、値はスカラーです",
        )),
        (Some(other), _) => Err(EvalError::new(format!(
            "型 {other} に対する norm の意味が定義されていません"
        ))),
        // 型が宣言されていない場合は、値の形に落とす。
        (None, Value::Scalar(x)) => Ok(Value::Scalar(x.abs())),
        (None, Value::Vector(items)) => Ok(Value::Scalar(euclidean_norm(items))),
    }
}

fn type_head(type_token: &str) -> String {
    type_token
        .split(['[', '('])
        .next()
        .unwrap_or(type_token)
        .trim()
        .to_owned()
}

fn euclidean_norm(items: &[f64]) -> f64 {
    items.iter().map(|x| x * x).sum::<f64>().sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_statement;

    fn eval_text(text: &str, env: &Env) -> Result<Value, EvalError> {
        let stmt = parse_statement(text).expect("parse");
        eval(stmt.body().expect("body"), env)
    }

    #[test]
    fn arithmetic_works() {
        let env = Env::new();
        assert_eq!(eval_text("1 + 2 * 3", &env), Ok(Value::Scalar(7.0)));
        assert_eq!(eval_text("2^3^2", &env), Ok(Value::Scalar(512.0)));
        assert_eq!(eval_text("-2 + 5", &env), Ok(Value::Scalar(3.0)));
    }

    #[test]
    fn variables_come_from_the_environment() {
        let mut env = Env::new();
        env.values.insert("x".to_owned(), Value::Scalar(4.0));
        assert_eq!(eval_text("x^2", &env), Ok(Value::Scalar(16.0)));
    }

    #[test]
    fn undefined_variable_is_an_error() {
        let env = Env::new();
        assert!(eval_text("q + 1", &env).is_err());
    }

    #[test]
    fn user_functions_are_applied() {
        let mut env = Env::new();
        let definition = match parse_statement("f(t) = t^2 + 1").expect("parse") {
            crate::ast::Stmt::FuncDef { params, body, .. } => FuncDefinition { params, body },
            other => panic!("想定外: {other:?}"),
        };
        env.functions.insert(("f".to_owned(), 1), definition);
        assert_eq!(eval_text("f(3)", &env), Ok(Value::Scalar(10.0)));
    }

    #[test]
    fn norm_dispatches_on_declared_type() {
        let mut env = Env::new();
        env.values
            .insert("v".to_owned(), Value::Vector(vec![3.0, 4.0]));
        env.types.insert("v".to_owned(), "Vector[Real]".to_owned());
        assert_eq!(eval_text("norm(v)", &env), Ok(Value::Scalar(5.0)));

        // 同じ値でも型宣言を Real に変えると意味が変わる（=エラーになる）。
        env.types.insert("v".to_owned(), "Real".to_owned());
        assert!(eval_text("norm(v)", &env).is_err());

        // スカラー + Real 宣言なら絶対値。
        env.values.insert("s".to_owned(), Value::Scalar(-3.0));
        env.types.insert("s".to_owned(), "Real".to_owned());
        assert_eq!(eval_text("norm(s)", &env), Ok(Value::Scalar(3.0)));
    }

    #[test]
    fn vector_arithmetic_works() {
        let mut env = Env::new();
        env.values
            .insert("u".to_owned(), Value::Vector(vec![1.0, 2.0]));
        env.values
            .insert("w".to_owned(), Value::Vector(vec![3.0, 4.0]));
        assert_eq!(
            eval_text("u + w", &env),
            Ok(Value::Vector(vec![4.0, 6.0]))
        );
        assert_eq!(eval_text("dot(u, w)", &env), Ok(Value::Scalar(11.0)));
        assert!(eval_text("u * w", &env).is_err());
    }

    #[test]
    fn deep_recursion_errors_instead_of_panicking() {
        let mut env = Env::new();
        let definition = match parse_statement("f(t) = f(t)").expect("parse") {
            crate::ast::Stmt::FuncDef { params, body, .. } => FuncDefinition { params, body },
            other => panic!("想定外: {other:?}"),
        };
        env.functions.insert(("f".to_owned(), 1), definition);
        assert!(eval_text("f(1)", &env).is_err());
    }
}
