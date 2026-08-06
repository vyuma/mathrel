//! 構文木。
//!
//! ホスト層の内部表現である。カーネルはこの中身を一切見ない
//! （カーネルにとっては不透明なバイト列）。

/// 二項演算子。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    /// 加算。
    Add,
    /// 減算。
    Sub,
    /// 乗算。
    Mul,
    /// 除算。
    Div,
    /// 冪。
    Pow,
}

impl BinOp {
    /// 表示用の記号。
    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Pow => "^",
        }
    }
}

/// 式。
#[derive(Clone, PartialEq, Debug)]
pub enum Ast {
    /// 数値リテラル。
    Number(f64),
    /// 名前参照。
    Variable(String),
    /// ベクトルリテラル。
    VectorLit(Vec<Ast>),
    /// 関数適用。
    Call(String, Vec<Ast>),
    /// 二項演算。
    Binary(BinOp, Box<Ast>, Box<Ast>),
    /// 単項マイナス。
    Negate(Box<Ast>),
}

impl Ast {
    /// 正規化された文字列表現。指紋計算に使う。
    ///
    /// 表示用ではないので、括弧を省略しない。
    #[must_use]
    pub fn canonical(&self) -> String {
        match self {
            Ast::Number(value) => format!("#{value:?}"),
            Ast::Variable(name) => format!("${name}"),
            Ast::VectorLit(items) => {
                let inner: Vec<String> = items.iter().map(Ast::canonical).collect();
                format!("[{}]", inner.join(","))
            }
            Ast::Call(name, args) => {
                let inner: Vec<String> = args.iter().map(Ast::canonical).collect();
                format!("{name}({})", inner.join(","))
            }
            Ast::Binary(op, left, right) => {
                format!("({}{}{})", left.canonical(), op.symbol(), right.canonical())
            }
            Ast::Negate(inner) => format!("(-{})", inner.canonical()),
        }
    }
}

/// 文。1 つのセルは 1 つの文になる。
#[derive(Clone, PartialEq, Debug)]
pub enum Stmt {
    /// `name = expr`
    ValueDef {
        /// 束縛される名前。
        name: String,
        /// 右辺。
        body: Ast,
    },
    /// `name(p, q) = expr`
    FuncDef {
        /// 関数名。
        name: String,
        /// 仮引数。
        params: Vec<String>,
        /// 本体。
        body: Ast,
    },
    /// `name : Type`
    TypeDecl {
        /// 対象の名前。
        name: String,
        /// 型トークン。カーネルにとっては不透明。
        type_token: String,
    },
    /// 名前を提供しない式。
    Anonymous {
        /// 式。
        body: Ast,
    },
}

impl Stmt {
    /// 宣言される名前があれば返す。
    #[must_use]
    pub fn declared_name(&self) -> Option<&str> {
        match self {
            Stmt::ValueDef { name, .. }
            | Stmt::FuncDef { name, .. }
            | Stmt::TypeDecl { name, .. } => Some(name),
            Stmt::Anonymous { .. } => None,
        }
    }

    /// 式の本体があれば返す。
    #[must_use]
    pub fn body(&self) -> Option<&Ast> {
        match self {
            Stmt::ValueDef { body, .. } | Stmt::FuncDef { body, .. } | Stmt::Anonymous { body } => {
                Some(body)
            }
            Stmt::TypeDecl { .. } => None,
        }
    }

    /// 種別を表す短い名前。
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Stmt::ValueDef { .. } => "value",
            Stmt::FuncDef { .. } => "function",
            Stmt::TypeDecl { .. } => "type",
            Stmt::Anonymous { .. } => "expression",
        }
    }
}
