//! LaTeX 風入力のパーサ。
//!
//! 完全な TeX 処理系ではない（企画書 §11 の恒久的な非目標）。
//! 数学用途に制約した部分集合を扱う。自由度を落とすことで、入力が
//! 解釈しやすく、依存関係が抽出しやすくなる。
//!
//! ## 受け付ける形
//!
//! ```text
//! x = 2                     値定義
//! f(t) = t^2 + 1            関数定義
//! v : Vector                型宣言
//! f(x) + 1                  無名の式
//! ```
//!
//! ## LaTeX 風の略記
//!
//! `\frac{a}{b}` `\sqrt{x}` `\norm{v}` `\cdot` `\times` `\pi` `\left(` `\right)`
//! `\sin` などは、前処理で素の構文に正規化される。

use crate::ast::{Ast, BinOp, Stmt};

/// パース失敗。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParseError {
    /// 人間向けの説明。
    pub message: String,
    /// 入力中の位置（バイト単位、正規化後）。
    pub position: usize,
}

impl ParseError {
    fn new(message: impl Into<String>, position: usize) -> Self {
        Self {
            message: message.into(),
            position,
        }
    }
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} (位置 {})", self.message, self.position)
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, PartialEq, Debug)]
enum Token {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Equals,
    Colon,
}

/// LaTeX 風の記法を素の構文へ正規化する。
///
/// ```
/// use mathrel_host::parser::normalize_latex;
/// assert_eq!(normalize_latex(r"\frac{1}{2}"), "((1)/(2))");
/// assert_eq!(normalize_latex(r"2 \cdot x"), "2 * x");
/// ```
#[must_use]
pub fn normalize_latex(input: &str) -> String {
    // 2 引数コマンドを先に処理する。`\frac{A}{B}` → `((A)/(B))`
    let mut text = replace_two_arg(input, "frac", |a, b| format!("(({a})/({b}))"));
    text = replace_two_arg(&text, "dfrac", |a, b| format!("(({a})/({b}))"));
    text = replace_two_arg(&text, "tfrac", |a, b| format!("(({a})/({b}))"));

    // 1 引数コマンド。
    for (command, function) in [
        ("sqrt", "sqrt"),
        ("norm", "norm"),
        ("abs", "abs"),
        ("lvert", "abs"),
    ] {
        text = replace_one_arg(&text, command, function);
    }

    // 記号の置換。長い名前から先に処理する。
    const SIMPLE: &[(&str, &str)] = &[
        (r"\left", ""),
        (r"\right", ""),
        (r"\cdot", "*"),
        (r"\times", "*"),
        (r"\div", "/"),
        (r"\pi", "pi"),
        (r"\infty", "inf"),
        (r"\arcsin", "asin"),
        (r"\arccos", "acos"),
        (r"\arctan", "atan"),
        (r"\sin", "sin"),
        (r"\cos", "cos"),
        (r"\tan", "tan"),
        (r"\exp", "exp"),
        (r"\ln", "ln"),
        (r"\log", "log"),
        (r"\min", "min"),
        (r"\max", "max"),
        (r"\mathbb{R}", "Real"),
        (r"\mathbb{N}", "Natural"),
        (r"\mathbb{Z}", "Integer"),
        (r"\mathbb{C}", "Complex"),
        (r"\lVert", "abs("),
        (r"\rVert", ")"),
        (r"\{", "["),
        (r"\}", "]"),
        (r"\,", " "),
        (r"\;", " "),
        (r"\!", ""),
    ];
    for (from, to) in SIMPLE {
        text = text.replace(from, to);
    }

    // `^{...}` → `^(...)`、`_{...}` → `_...`（添字は名前の一部として扱う）。
    text = rewrite_braces_after(&text, '^');
    text = strip_braces_after(&text, '_');
    text
}

/// `\name{A}{B}` を探して置換する。
fn replace_two_arg<F>(input: &str, name: &str, build: F) -> String
where
    F: Fn(&str, &str) -> String,
{
    let needle = format!("\\{name}");
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find(&needle) {
        output.push_str(&rest[..index]);
        let after = &rest[index + needle.len()..];
        match (
            take_braced(after),
            take_braced(after).and_then(|(_, tail)| take_braced(tail)),
        ) {
            (Some((first, _)), Some((second, tail))) => {
                output.push_str(&build(&normalize_latex(first), &normalize_latex(second)));
                rest = tail;
            }
            _ => {
                // 引数が揃っていない。そのまま残して先へ進む。
                output.push_str(&needle);
                rest = after;
            }
        }
    }
    output.push_str(rest);
    output
}

/// `\name{A}` を `function(A)` に置換する。
fn replace_one_arg(input: &str, name: &str, function: &str) -> String {
    let needle = format!("\\{name}");
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find(&needle) {
        output.push_str(&rest[..index]);
        let after = &rest[index + needle.len()..];
        match take_braced(after) {
            Some((inner, tail)) => {
                output.push_str(function);
                output.push('(');
                output.push_str(&normalize_latex(inner));
                output.push(')');
                rest = tail;
            }
            None => {
                output.push_str(function);
                rest = after;
            }
        }
    }
    output.push_str(rest);
    output
}

/// 先頭の `{...}` を取り出す。入れ子に対応する。
fn take_braced(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim_start();
    let offset = input.len() - trimmed.len();
    if !trimmed.starts_with('{') {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&trimmed[1..index], &input[offset + index + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// `^{...}` を `^(...)` に書き換える。
fn rewrite_braces_after(input: &str, marker: char) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find(marker) {
        output.push_str(&rest[..index]);
        output.push(marker);
        let after = &rest[index + marker.len_utf8()..];
        match take_braced(after) {
            Some((inner, tail)) => {
                output.push('(');
                output.push_str(inner);
                output.push(')');
                rest = tail;
            }
            None => rest = after,
        }
    }
    output.push_str(rest);
    output
}

/// `marker{...}` の波括弧を外す。`x_{1}` → `x_1`。
///
/// 添字は名前の一部として扱うので、`^` と違って括弧を残さない。MathLive は
/// 添字を必ず `_{...}` の形で出すため、これがないと UI からの入力が
/// そのままではパースできない。
fn strip_braces_after(input: &str, marker: char) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find(marker) {
        output.push_str(&rest[..index]);
        output.push(marker);
        let after = &rest[index + marker.len_utf8()..];
        match take_braced(after) {
            Some((inner, tail)) => {
                output.push_str(inner);
                rest = tail;
            }
            None => rest = after,
        }
    }
    output.push_str(rest);
    output
}

fn tokenize(input: &str) -> Result<Vec<(usize, Token)>, ParseError> {
    let mut tokens = Vec::new();
    let bytes: Vec<char> = input.chars().collect();
    let mut index = 0usize;

    while index < bytes.len() {
        let start = index;
        let ch = bytes[index];
        match ch {
            c if c.is_whitespace() => {
                index += 1;
            }
            '+' => {
                tokens.push((start, Token::Plus));
                index += 1;
            }
            '-' => {
                tokens.push((start, Token::Minus));
                index += 1;
            }
            '*' => {
                tokens.push((start, Token::Star));
                index += 1;
            }
            '/' => {
                tokens.push((start, Token::Slash));
                index += 1;
            }
            '^' => {
                tokens.push((start, Token::Caret));
                index += 1;
            }
            '(' => {
                tokens.push((start, Token::LParen));
                index += 1;
            }
            ')' => {
                tokens.push((start, Token::RParen));
                index += 1;
            }
            '[' => {
                tokens.push((start, Token::LBracket));
                index += 1;
            }
            ']' => {
                tokens.push((start, Token::RBracket));
                index += 1;
            }
            ',' => {
                tokens.push((start, Token::Comma));
                index += 1;
            }
            '=' => {
                tokens.push((start, Token::Equals));
                index += 1;
            }
            ':' => {
                tokens.push((start, Token::Colon));
                index += 1;
            }
            c if c.is_ascii_digit() || c == '.' => {
                let mut text = String::new();
                while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == '.')
                {
                    text.push(bytes[index]);
                    index += 1;
                }
                // 指数表記。
                if index < bytes.len() && (bytes[index] == 'e' || bytes[index] == 'E') {
                    let mut lookahead = index + 1;
                    if lookahead < bytes.len()
                        && (bytes[lookahead] == '+' || bytes[lookahead] == '-')
                    {
                        lookahead += 1;
                    }
                    if lookahead < bytes.len() && bytes[lookahead].is_ascii_digit() {
                        while index < lookahead {
                            text.push(bytes[index]);
                            index += 1;
                        }
                        while index < bytes.len() && bytes[index].is_ascii_digit() {
                            text.push(bytes[index]);
                            index += 1;
                        }
                    }
                }
                let value = text
                    .parse::<f64>()
                    .map_err(|_| ParseError::new(format!("数値として読めません: {text}"), start))?;
                tokens.push((start, Token::Number(value)));
            }
            c if c.is_alphabetic() || c == '\\' || c == '_' => {
                let mut text = String::new();
                if c == '\\' {
                    index += 1;
                }
                while index < bytes.len() && (bytes[index].is_alphanumeric() || bytes[index] == '_')
                {
                    text.push(bytes[index]);
                    index += 1;
                }
                if text.is_empty() {
                    return Err(ParseError::new(format!("読めない文字: {ch}"), start));
                }
                tokens.push((start, Token::Ident(text)));
            }
            other => {
                return Err(ParseError::new(format!("読めない文字: {other}"), start));
            }
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<(usize, Token)>,
    cursor: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.cursor).map(|(_, token)| token)
    }

    fn position(&self) -> usize {
        self.tokens
            .get(self.cursor)
            .map(|(position, _)| *position)
            .unwrap_or_else(|| self.tokens.last().map(|(p, _)| *p).unwrap_or(0))
    }

    fn advance(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.cursor).map(|(_, token)| token.clone());
        if token.is_some() {
            self.cursor += 1;
        }
        token
    }

    fn expect(&mut self, expected: &Token, what: &str) -> Result<(), ParseError> {
        if self.peek() == Some(expected) {
            self.cursor += 1;
            Ok(())
        } else {
            Err(ParseError::new(
                format!("{what} が必要です"),
                self.position(),
            ))
        }
    }

    fn parse_expr(&mut self) -> Result<Ast, ParseError> {
        let mut left = self.parse_term()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => BinOp::Add,
                Some(Token::Minus) => BinOp::Sub,
                _ => break,
            };
            self.cursor += 1;
            let right = self.parse_term()?;
            left = Ast::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_term(&mut self) -> Result<Ast, ParseError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => BinOp::Mul,
                Some(Token::Slash) => BinOp::Div,
                // 暗黙の乗算: `2x`, `2(x+1)`, `x y`
                Some(Token::Number(_)) | Some(Token::Ident(_)) | Some(Token::LParen) => {
                    let right = self.parse_unary()?;
                    left = Ast::Binary(BinOp::Mul, Box::new(left), Box::new(right));
                    continue;
                }
                _ => break,
            };
            self.cursor += 1;
            let right = self.parse_unary()?;
            left = Ast::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Ast, ParseError> {
        if self.peek() == Some(&Token::Minus) {
            self.cursor += 1;
            let inner = self.parse_unary()?;
            return Ok(Ast::Negate(Box::new(inner)));
        }
        self.parse_power()
    }

    fn parse_power(&mut self) -> Result<Ast, ParseError> {
        let base = self.parse_primary()?;
        if self.peek() == Some(&Token::Caret) {
            self.cursor += 1;
            // 冪は右結合。
            let exponent = self.parse_unary()?;
            return Ok(Ast::Binary(BinOp::Pow, Box::new(base), Box::new(exponent)));
        }
        Ok(base)
    }

    fn parse_primary(&mut self) -> Result<Ast, ParseError> {
        let position = self.position();
        match self.advance() {
            Some(Token::Number(value)) => Ok(Ast::Number(value)),
            Some(Token::Ident(name)) => {
                if self.peek() == Some(&Token::LParen) {
                    self.cursor += 1;
                    let mut args = Vec::new();
                    if self.peek() != Some(&Token::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if self.peek() == Some(&Token::Comma) {
                                self.cursor += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(&Token::RParen, "閉じ括弧 )")?;
                    Ok(Ast::Call(name, args))
                } else {
                    Ok(Ast::Variable(name))
                }
            }
            Some(Token::LParen) => {
                let inner = self.parse_expr()?;
                self.expect(&Token::RParen, "閉じ括弧 )")?;
                Ok(inner)
            }
            Some(Token::LBracket) => {
                let mut items = Vec::new();
                if self.peek() != Some(&Token::RBracket) {
                    loop {
                        items.push(self.parse_expr()?);
                        if self.peek() == Some(&Token::Comma) {
                            self.cursor += 1;
                        } else {
                            break;
                        }
                    }
                }
                self.expect(&Token::RBracket, "閉じ括弧 ]")?;
                Ok(Ast::VectorLit(items))
            }
            Some(other) => Err(ParseError::new(
                format!("式が必要ですが {other:?} がありました"),
                position,
            )),
            None => Err(ParseError::new("式が途中で終わっています", position)),
        }
    }

    fn at_end(&self) -> bool {
        self.cursor >= self.tokens.len()
    }
}

/// 1 行を [`Stmt`] にする。
///
/// ```
/// use mathrel_host::parser::parse_statement;
/// use mathrel_host::ast::Stmt;
///
/// let stmt = parse_statement("f(t) = t^2 + 1").expect("parse");
/// assert!(matches!(stmt, Stmt::FuncDef { .. }));
/// ```
pub fn parse_statement(input: &str) -> Result<Stmt, ParseError> {
    let normalized = normalize_latex(input);
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return Err(ParseError::new("空の入力です", 0));
    }
    let tokens = tokenize(trimmed)?;
    if tokens.is_empty() {
        return Err(ParseError::new("空の入力です", 0));
    }

    // 型宣言: `name : Type`
    if let Some(colon) = tokens.iter().position(|(_, token)| *token == Token::Colon) {
        if colon == 1 {
            if let Token::Ident(name) = &tokens[0].1 {
                let type_token: String = tokens[colon + 1..]
                    .iter()
                    .map(|(_, token)| match token {
                        Token::Ident(text) => text.clone(),
                        Token::LBracket => "[".to_owned(),
                        Token::RBracket => "]".to_owned(),
                        Token::Comma => ",".to_owned(),
                        Token::Number(value) => format!("{value}"),
                        other => format!("{other:?}"),
                    })
                    .collect();
                if type_token.is_empty() {
                    return Err(ParseError::new("型が空です", tokens[colon].0));
                }
                return Ok(Stmt::TypeDecl {
                    name: name.clone(),
                    type_token,
                });
            }
        }
        return Err(ParseError::new(
            "型宣言は `名前 : 型` の形で書いてください",
            tokens[colon].0,
        ));
    }

    // 定義: 最初の `=` で分割する。
    if let Some(equals) = tokens.iter().position(|(_, token)| *token == Token::Equals) {
        let lhs = &tokens[..equals];
        let rhs = &tokens[equals + 1..];
        if rhs.is_empty() {
            return Err(ParseError::new("右辺がありません", tokens[equals].0));
        }
        let mut body_parser = Parser {
            tokens: rhs.to_vec(),
            cursor: 0,
        };
        let body = body_parser.parse_expr()?;
        if !body_parser.at_end() {
            return Err(ParseError::new(
                "右辺に余分なものがあります",
                body_parser.position(),
            ));
        }

        // `name = ...`
        if lhs.len() == 1 {
            if let Token::Ident(name) = &lhs[0].1 {
                return Ok(Stmt::ValueDef {
                    name: name.clone(),
                    body,
                });
            }
        }
        // `name(p, q) = ...`
        if lhs.len() >= 3 {
            if let (Token::Ident(name), Token::LParen) = (&lhs[0].1, &lhs[1].1) {
                if lhs[lhs.len() - 1].1 == Token::RParen {
                    let mut params = Vec::new();
                    let mut expect_ident = true;
                    for (position, token) in &lhs[2..lhs.len() - 1] {
                        match token {
                            Token::Ident(param) if expect_ident => {
                                params.push(param.clone());
                                expect_ident = false;
                            }
                            Token::Comma if !expect_ident => expect_ident = true,
                            _ => {
                                return Err(ParseError::new(
                                    "仮引数は名前をカンマ区切りで書いてください",
                                    *position,
                                ))
                            }
                        }
                    }
                    if params.is_empty() {
                        return Err(ParseError::new("仮引数がありません", lhs[1].0));
                    }
                    if params.len() > u8::MAX as usize {
                        return Err(ParseError::new("仮引数が多すぎます", lhs[1].0));
                    }
                    return Ok(Stmt::FuncDef {
                        name: name.clone(),
                        params,
                        body,
                    });
                }
            }
        }
        return Err(ParseError::new(
            "左辺は `名前` か `名前(引数...)` の形で書いてください",
            lhs.first().map(|(position, _)| *position).unwrap_or(0),
        ));
    }

    // 無名の式。
    let mut parser = Parser { tokens, cursor: 0 };
    let body = parser.parse_expr()?;
    if !parser.at_end() {
        return Err(ParseError::new(
            "式に余分なものがあります",
            parser.position(),
        ));
    }
    Ok(Stmt::Anonymous { body })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MathLive は添字を `_{...}` の形で出す。名前の一部として取り込む。
    #[test]
    fn braced_subscripts_become_part_of_the_name() {
        assert_eq!(normalize_latex("x_{1}"), "x_1");
        assert_eq!(normalize_latex("a_{ij} + b_{k}"), "a_ij + b_k");

        let stmt = parse_statement("x_{1} = 2").expect("parse");
        assert_eq!(
            stmt,
            Stmt::ValueDef {
                name: "x_1".to_owned(),
                body: Ast::Number(2.0),
            }
        );
    }

    /// 添字つきの名前は、括弧の有無にかかわらず同じものを指す。
    #[test]
    fn braced_and_bare_subscripts_agree() {
        assert_eq!(
            parse_statement("x_{1} = 2").expect("parse"),
            parse_statement("x_1 = 2").expect("parse")
        );
    }

    /// 指数は名前の一部にせず、括弧を残す。添字とは扱いが違う。
    #[test]
    fn braced_exponents_keep_their_grouping() {
        assert_eq!(normalize_latex("x^{2}"), "x^(2)");
        assert_eq!(normalize_latex("x^{n+1}"), "x^(n+1)");
    }

    #[test]
    fn a_lone_underscore_is_left_alone() {
        assert_eq!(normalize_latex("a_b"), "a_b");
        assert_eq!(normalize_latex("x_"), "x_");
    }

    #[test]
    fn parses_value_definition() {
        let stmt = parse_statement("x = 2").expect("parse");
        assert_eq!(
            stmt,
            Stmt::ValueDef {
                name: "x".to_owned(),
                body: Ast::Number(2.0),
            }
        );
    }

    #[test]
    fn parses_function_definition() {
        let stmt = parse_statement("f(t) = t^2 + 1").expect("parse");
        match stmt {
            Stmt::FuncDef { name, params, .. } => {
                assert_eq!(name, "f");
                assert_eq!(params, vec!["t".to_owned()]);
            }
            other => panic!("想定外: {other:?}"),
        }
    }

    #[test]
    fn parses_type_declaration() {
        let stmt = parse_statement("v : Vector[Real]").expect("parse");
        assert_eq!(
            stmt,
            Stmt::TypeDecl {
                name: "v".to_owned(),
                type_token: "Vector[Real]".to_owned(),
            }
        );
    }

    #[test]
    fn power_is_right_associative() {
        let stmt = parse_statement("2^3^2").expect("parse");
        assert_eq!(stmt.body().expect("body").canonical(), "(#2.0^(#3.0^#2.0))");
    }

    #[test]
    fn implicit_multiplication_works() {
        let stmt = parse_statement("y = 2x").expect("parse");
        assert_eq!(stmt.body().expect("body").canonical(), "(#2.0*$x)");
    }

    #[test]
    fn latex_fraction_is_normalized() {
        let stmt = parse_statement(r"y = \frac{1}{2}").expect("parse");
        assert_eq!(stmt.body().expect("body").canonical(), "(#1.0/#2.0)");
    }

    #[test]
    fn latex_sqrt_and_cdot() {
        let stmt = parse_statement(r"y = 2 \cdot \sqrt{9}").expect("parse");
        assert_eq!(stmt.body().expect("body").canonical(), "(#2.0*sqrt(#9.0))");
    }

    #[test]
    fn vector_literal_parses() {
        let stmt = parse_statement("v = [1, 2, 3]").expect("parse");
        assert_eq!(stmt.body().expect("body").canonical(), "[#1.0,#2.0,#3.0]");
    }

    #[test]
    fn missing_right_hand_side_is_an_error() {
        assert!(parse_statement("x =").is_err());
    }

    #[test]
    fn unbalanced_parenthesis_is_an_error() {
        assert!(parse_statement("y = (1 + 2").is_err());
    }
}
