//! ワークスペース — カーネルとホストを繋ぐ層。
//!
//! ユーザから見える単位は「セル」である。1 セル = 1 文 = 1 カーネル Entity。
//!
//! ## 評価環境は依存グラフから作る
//!
//! グローバルなシンボルテーブルを別に持たない。あるセルを評価するときの
//! 環境は、そのセルの**上流の推移閉包**から組み立てる（[`Workspace::build_env`]）。
//! カーネルが追跡している依存と、評価で実際に使われる束縛が構造的に一致する
//! ため、「依存として登録されていない値をこっそり読む」経路が存在しない。

use crate::ast::Stmt;
use crate::eval::{eval, Env, FuncDefinition};
use crate::extract::{extract, CapabilitySpec};
use crate::json::{self, Object};
use crate::parser::parse_statement;
use crate::value::{digest_of, digest_of_value, Value};
use mathrel_kernel::{
    Capability, Decl, DeclKind, Entity, EvalOutcome, Expr, Freshness, ItemSpec, Kernel, TypeInfo,
    ValueUpdate,
};
use std::collections::{BTreeSet, HashMap, VecDeque};

/// セルの識別子。UI 側で安定して使える。
pub type CellId = u64;

/// ワークスペース操作の失敗。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorkspaceError {
    /// 指定されたセルがない。
    UnknownCell(CellId),
    /// カーネルが操作を拒否した。
    Kernel(String),
}

impl core::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WorkspaceError::UnknownCell(id) => write!(f, "セル {id} がありません"),
            WorkspaceError::Kernel(message) => write!(f, "カーネルエラー: {message}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

/// 1 つのセル。
#[derive(Clone, Debug)]
pub struct Cell {
    /// 識別子。
    pub id: CellId,
    /// 対応するカーネル Entity。
    pub entity: Entity,
    /// ユーザが入力した原文字列。
    pub source: String,
    /// パース結果。失敗していれば `None`。
    pub stmt: Option<Stmt>,
    /// パース失敗の説明。
    pub parse_error: Option<String>,
}

/// 評価の統計。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvalStats {
    /// 評価に成功した件数。
    pub evaluated: usize,
    /// 評価に失敗した件数。
    pub failed: usize,
    /// 評価した順序。
    pub order: Vec<CellId>,
}

/// 数学ワークスペース。
#[derive(Debug)]
pub struct Workspace {
    kernel: Kernel,
    cells: Vec<Cell>,
    index: HashMap<Entity, usize>,
    values: HashMap<Entity, Value>,
    next_id: CellId,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspace {
    /// 空のワークスペース。
    #[must_use]
    pub fn new() -> Self {
        Self {
            kernel: Kernel::new(),
            cells: Vec::new(),
            index: HashMap::new(),
            values: HashMap::new(),
            next_id: 1,
        }
    }

    /// 内側のカーネル。
    #[must_use]
    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    /// セル一覧（表示順）。
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// セルを引く。
    #[must_use]
    pub fn cell(&self, id: CellId) -> Option<&Cell> {
        self.cells.iter().find(|cell| cell.id == id)
    }

    /// セルの現在値。`Clean` でなければ `None`。
    #[must_use]
    pub fn value(&self, id: CellId) -> Option<&Value> {
        let cell = self.cell(id)?;
        match self.kernel.freshness(cell.entity) {
            Ok(Freshness::Clean { .. }) => self.values.get(&cell.entity),
            _ => None,
        }
    }

    // ---------------------------------------------------------------
    // 編集
    // ---------------------------------------------------------------

    /// セルを追加する。
    pub fn add_cell(&mut self, source: &str) -> CellId {
        let (stmt, parse_error) = parse(source);
        let (provides, requires) = self.capabilities_for(stmt.as_ref());
        let spec = ItemSpec {
            decl: stmt.as_ref().and_then(|stmt| self.decl_for(stmt)),
            expr: Some(Expr::from_text(
                &stmt
                    .as_ref()
                    .and_then(|stmt| stmt.body().map(|body| body.canonical()))
                    .unwrap_or_default(),
            )),
            provides,
            requires,
            type_info: type_info_for(stmt.as_ref()),
            display: None,
            source: Some(source.to_owned()),
        };

        // `add_item` は Entity を確保するだけで、失敗する経路がない。
        let entity = match self.kernel.add_item(spec) {
            Ok((entity, _report)) => entity,
            Err(_) => return 0,
        };

        let id = self.next_id;
        self.next_id += 1;
        self.index.insert(entity, self.cells.len());
        self.cells.push(Cell {
            id,
            entity,
            source: source.to_owned(),
            stmt,
            parse_error,
        });
        id
    }

    /// セルの中身を差し替える。
    pub fn update_cell(&mut self, id: CellId, source: &str) -> Result<(), WorkspaceError> {
        let position = self
            .cells
            .iter()
            .position(|cell| cell.id == id)
            .ok_or(WorkspaceError::UnknownCell(id))?;
        let entity = self.cells[position].entity;

        let (stmt, parse_error) = parse(source);
        let (provides, requires) = self.capabilities_for(stmt.as_ref());
        let update = ValueUpdate {
            expr: Some(Expr::from_text(
                &stmt
                    .as_ref()
                    .and_then(|stmt| stmt.body().map(|body| body.canonical()))
                    .unwrap_or_default(),
            )),
            provides: Some(provides),
            requires: Some(requires),
            source: Some(source.to_owned()),
            decl: stmt.as_ref().and_then(|stmt| self.decl_for(stmt)),
        };

        self.kernel
            .change_value(entity, update)
            .map_err(|error| WorkspaceError::Kernel(error.to_string()))?;

        let cell = &mut self.cells[position];
        cell.source = source.to_owned();
        cell.stmt = stmt;
        cell.parse_error = parse_error;
        Ok(())
    }

    /// セルを削除する。
    ///
    /// 依存していたセルは削除されない。未解決状態になって残る。
    pub fn remove_cell(&mut self, id: CellId) -> Result<(), WorkspaceError> {
        let position = self
            .cells
            .iter()
            .position(|cell| cell.id == id)
            .ok_or(WorkspaceError::UnknownCell(id))?;
        let entity = self.cells[position].entity;

        self.kernel
            .remove_item(entity)
            .map_err(|error| WorkspaceError::Kernel(error.to_string()))?;

        self.cells.remove(position);
        self.values.remove(&entity);
        self.reindex();
        Ok(())
    }

    // ---------------------------------------------------------------
    // 評価
    // ---------------------------------------------------------------

    /// 再計算が必要なセルを、依存順にすべて評価する。
    ///
    /// カーネルが `next_batch()` で返したものだけを評価する。何を評価すべきか
    /// を決めるのはカーネルであり、ホストではない。
    pub fn evaluate(&mut self) -> EvalStats {
        let mut stats = EvalStats::default();
        loop {
            let batch = self.kernel.next_batch();
            if batch.is_empty() {
                break;
            }
            let mut progressed = false;
            for entity in batch {
                let (outcome, value) = self.compute(entity);
                match value {
                    Some(value) => {
                        self.values.insert(entity, value);
                    }
                    None => {
                        self.values.remove(&entity);
                    }
                }
                let succeeded = matches!(outcome, EvalOutcome::Value { .. });
                if self.kernel.commit_evaluation(entity, outcome).is_ok() {
                    progressed = true;
                    if succeeded {
                        stats.evaluated += 1;
                    } else {
                        stats.failed += 1;
                    }
                    if let Some(cell) = self.cell_of(entity) {
                        stats.order.push(cell.id);
                    }
                }
            }
            if !progressed {
                break;
            }
        }
        stats
    }

    /// 1 つのエンティティを評価する。カーネルの状態は変えない。
    fn compute(&self, entity: Entity) -> (EvalOutcome, Option<Value>) {
        let cell = match self.cell_of(entity) {
            Some(cell) => cell,
            None => {
                return (
                    EvalOutcome::Failed {
                        reason: "対応するセルがありません".to_owned(),
                    },
                    None,
                )
            }
        };

        if let Some(error) = &cell.parse_error {
            return (
                EvalOutcome::Failed {
                    reason: error.clone(),
                },
                None,
            );
        }

        let stmt = match &cell.stmt {
            Some(stmt) => stmt,
            None => {
                return (
                    EvalOutcome::Failed {
                        reason: "文がありません".to_owned(),
                    },
                    None,
                )
            }
        };

        match stmt {
            Stmt::TypeDecl { type_token, .. } => (
                EvalOutcome::Value {
                    digest: digest_of(&["type", type_token]),
                },
                None,
            ),
            Stmt::FuncDef { params, body, .. } => {
                // 関数定義の指紋は、本体の形と、その本体が参照している上流の
                // 指紋から作る。自由変数 `c` が変われば f の指紋も変わり、
                // f を使うセルへ dirty が伝わる。
                let mut parts: Vec<String> = vec![
                    "func".to_owned(),
                    params.join(","),
                    body.canonical(),
                ];
                for upstream in self.kernel.dependencies(entity).unwrap_or_default() {
                    parts.push(self.upstream_fingerprint(upstream));
                }
                let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
                (
                    EvalOutcome::Value {
                        digest: digest_of(&refs),
                    },
                    None,
                )
            }
            Stmt::ValueDef { body, .. } | Stmt::Anonymous { body } => {
                let env = self.build_env(entity);
                match eval(body, &env) {
                    Ok(value) => (
                        EvalOutcome::Value {
                            digest: digest_of_value(&value),
                        },
                        Some(value),
                    ),
                    Err(error) => (
                        EvalOutcome::Failed {
                            reason: error.message,
                        },
                        None,
                    ),
                }
            }
        }
    }

    fn upstream_fingerprint(&self, entity: Entity) -> String {
        match self.kernel.freshness(entity) {
            Ok(Freshness::Clean { digest, .. }) => digest.iter().map(|b| format!("{b:02x}")).collect(),
            _ => "?".to_owned(),
        }
    }

    /// 評価環境を上流の推移閉包から組み立てる。
    ///
    /// 推移閉包を取るのは、関数定義が自身の自由変数を必要とするからである。
    /// `f(t) = t^2 + c` を `y = f(2)` から呼ぶとき、`y` の直接の上流は `f` だが、
    /// 評価には `c` が要る。`y → f → c` という鎖はカーネルが把握しているので、
    /// 閉包を辿れば必ず届く。
    fn build_env(&self, entity: Entity) -> Env {
        let mut env = Env::new();
        let mut visited: BTreeSet<Entity> = BTreeSet::new();
        let mut queue: VecDeque<Entity> = self
            .kernel
            .dependencies(entity)
            .unwrap_or_default()
            .into_iter()
            .collect();

        // 決定的な順序で走査する。曖昧参照（同名の provider が複数）の場合、
        // index の小さい方が勝つ。カーネルは別途 Ambiguous を報告している。
        while let Some(current) = queue.pop_front() {
            if current == entity || !visited.insert(current) {
                continue;
            }
            let cell = match self.cell_of(current) {
                Some(cell) => cell,
                None => continue,
            };
            match &cell.stmt {
                Some(Stmt::ValueDef { name, .. }) => {
                    if let Some(value) = self.values.get(&current) {
                        env.values.entry(name.clone()).or_insert_with(|| value.clone());
                    }
                }
                Some(Stmt::FuncDef { name, params, body }) => {
                    env.functions
                        .entry((name.clone(), params.len()))
                        .or_insert_with(|| FuncDefinition {
                            params: params.clone(),
                            body: body.clone(),
                        });
                }
                Some(Stmt::TypeDecl { name, type_token }) => {
                    env.types
                        .entry(name.clone())
                        .or_insert_with(|| type_token.clone());
                }
                Some(Stmt::Anonymous { .. }) | None => {}
            }
            let mut next = self.kernel.dependencies(current).unwrap_or_default();
            next.sort_unstable();
            for upstream in next {
                if !visited.contains(&upstream) {
                    queue.push_back(upstream);
                }
            }
        }
        env
    }

    // ---------------------------------------------------------------
    // 状態の書き出し
    // ---------------------------------------------------------------

    /// UI 向けのスナップショットを JSON で返す。
    #[must_use]
    pub fn to_json(&self) -> String {
        let cells: Vec<String> = self.cells.iter().map(|cell| self.cell_json(cell)).collect();
        let cycles: Vec<String> = self
            .kernel
            .cycles()
            .into_iter()
            .map(|cycle| {
                let ids: Vec<String> = cycle
                    .into_iter()
                    .filter_map(|entity| self.cell_of(entity).map(|cell| cell.id.to_string()))
                    .collect();
                format!("[{}]", ids.join(","))
            })
            .collect();

        Object::new()
            .number("revision", self.kernel.revision().0 as f64)
            .array("cells", cells)
            .array("cycles", cycles)
            .build()
    }

    fn cell_json(&self, cell: &Cell) -> String {
        let entity = cell.entity;
        let freshness = self
            .kernel
            .freshness(entity)
            .map(|freshness| freshness.kind_str())
            .unwrap_or("Unknown");
        let empty = mathrel_kernel::Resolution::Resolved;
        let resolution = self.kernel.resolution(entity).unwrap_or(&empty);

        let missing: Vec<String> = resolution
            .missing()
            .iter()
            .map(|capability| self.kernel.capability_label(*capability))
            .collect();
        let conflicts: Vec<String> = resolution
            .conflicts()
            .iter()
            .map(|(capability, _)| self.kernel.capability_label(*capability))
            .collect();
        let provides: Vec<String> = self
            .kernel
            .provides(entity)
            .iter()
            .map(|capability| self.kernel.capability_label(*capability))
            .collect();
        let requires: Vec<String> = self
            .kernel
            .requires(entity)
            .iter()
            .map(|capability| self.kernel.capability_label(*capability))
            .collect();
        let dependencies: Vec<String> = self
            .kernel
            .dependencies(entity)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|upstream| self.cell_of(upstream).map(|cell| cell.id.to_string()))
            .collect();
        let dependents: Vec<String> = self
            .kernel
            .dependents(entity)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|downstream| self.cell_of(downstream).map(|cell| cell.id.to_string()))
            .collect();

        let value = self.value(cell.id).map(Value::render);
        let value_type = self.value(cell.id).map(|value| value.type_name().to_owned());
        let failure = self
            .kernel
            .failure(entity)
            .ok()
            .flatten()
            .map(str::to_owned)
            .or_else(|| cell.parse_error.clone());

        Object::new()
            .number("id", cell.id as f64)
            .string("source", &cell.source)
            .string(
                "kind",
                cell.stmt.as_ref().map_or("invalid", Stmt::kind_str),
            )
            .optional_string("name", cell.stmt.as_ref().and_then(Stmt::declared_name))
            .string("freshness", freshness)
            .string("resolution", resolution.kind_str())
            .boolean(
                "inCycle",
                self.kernel.in_cycle(entity).unwrap_or(false),
            )
            .optional_string("value", value.as_deref())
            .optional_string("valueType", value_type.as_deref())
            .optional_string("error", failure.as_deref())
            .string_array("missing", &missing)
            .string_array("conflicts", &conflicts)
            .string_array("provides", &provides)
            .string_array("requires", &requires)
            .raw("dependencies", format!("[{}]", dependencies.join(",")))
            .raw("dependents", format!("[{}]", dependents.join(",")))
            .build()
    }

    /// 直近の評価統計を JSON で返す。
    #[must_use]
    pub fn stats_json(stats: &EvalStats) -> String {
        Object::new()
            .number("evaluated", stats.evaluated as f64)
            .number("failed", stats.failed as f64)
            .raw(
                "order",
                format!(
                    "[{}]",
                    stats
                        .order
                        .iter()
                        .map(CellId::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            )
            .build()
    }

    /// JSON 文字列としてエスケープする（wasm 層から使う）。
    #[must_use]
    pub fn quote(text: &str) -> String {
        json::quote(text)
    }

    // ---------------------------------------------------------------
    // 内部
    // ---------------------------------------------------------------

    fn cell_of(&self, entity: Entity) -> Option<&Cell> {
        self.index
            .get(&entity)
            .and_then(|position| self.cells.get(*position))
            .filter(|cell| cell.entity == entity)
            .or_else(|| self.cells.iter().find(|cell| cell.entity == entity))
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (position, cell) in self.cells.iter().enumerate() {
            self.index.insert(cell.entity, position);
        }
    }

    fn capabilities_for(&mut self, stmt: Option<&Stmt>) -> (Vec<Capability>, Vec<Capability>) {
        let spec = match stmt {
            Some(stmt) => extract(stmt),
            None => return (Vec::new(), Vec::new()),
        };
        let provides = spec
            .provides
            .iter()
            .map(|capability| self.intern_capability(capability))
            .collect();
        let requires = spec
            .requires
            .iter()
            .map(|capability| self.intern_capability(capability))
            .collect();
        (provides, requires)
    }

    fn intern_capability(&mut self, spec: &CapabilitySpec) -> Capability {
        match spec {
            CapabilitySpec::NameBound(name) => Capability::NameBound(self.kernel.intern(name)),
            CapabilitySpec::FunctionBound { name, arity } => Capability::FunctionBound {
                name: self.kernel.intern(name),
                arity: *arity,
            },
            CapabilitySpec::TypeKnown(name) => Capability::TypeKnown(self.kernel.intern(name)),
        }
    }

    fn decl_for(&mut self, stmt: &Stmt) -> Option<Decl> {
        let (name, kind) = match stmt {
            Stmt::ValueDef { name, .. } => (name.clone(), DeclKind::Value),
            Stmt::FuncDef { name, params, .. } => (
                name.clone(),
                DeclKind::Function {
                    arity: u8::try_from(params.len()).unwrap_or(u8::MAX),
                },
            ),
            Stmt::TypeDecl { name, .. } => (name.clone(), DeclKind::Value),
            Stmt::Anonymous { .. } => return None,
        };
        Some(Decl {
            name: self.kernel.intern(&name),
            kind,
        })
    }
}

fn parse(source: &str) -> (Option<Stmt>, Option<String>) {
    match parse_statement(source) {
        Ok(stmt) => (Some(stmt), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn type_info_for(stmt: Option<&Stmt>) -> Option<TypeInfo> {
    match stmt {
        Some(Stmt::TypeDecl { type_token, .. }) => Some(TypeInfo {
            type_token: Some(type_token.clone()),
        }),
        _ => None,
    }
}
