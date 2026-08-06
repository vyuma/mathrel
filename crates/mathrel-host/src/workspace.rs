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
//!
//! ## 数式と証明は同じセルの列に載る
//!
//! `y = f(x)` が古くなるのと、`定理: f は非負` が古くなるのは同じ現象である。
//! したがってグラフを 2 つ持つ理由がない（`docs/検証層の設計.md` ADR-006）。
//!
//! セルには 2 種類あり（[`CellKind`]）、どちらも同じ `next_batch` ループで
//! 回る。数式セルはホストの評価器が、検証義務は [`Verifier`] が処理する。
//! 定理が `uses` で挙げた名前は、その名前を定義しているセルへの依存になる。

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
use mathrel_verify::{Obligation, Trust, Verdict, Verifier};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::rc::Rc;

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

/// セルの中身の種別。
#[derive(Clone, PartialEq, Debug)]
pub enum CellKind {
    /// 数式・定義。ホストの評価器が処理する。
    Expression,
    /// 検証義務。[`Verifier`] が処理する。
    Obligation {
        /// 命題と、その依存の申告。
        obligation: Box<Obligation>,
        /// 人間が「既知」と宣言したもの。検証器へは渡さない。
        ///
        /// 仮置きかどうかは**義務そのものの属性**であって、どの道具で検査
        /// するかとは無関係である（`docs/検証層の設計.md` §6.0(f)）。
        assumed: bool,
        /// 直近の判定。未検証なら `None`。
        verdict: Option<Verdict>,
    },
}

/// 1 つのセル。
#[derive(Clone, Debug)]
pub struct Cell {
    /// 識別子。
    pub id: CellId,
    /// 対応するカーネル Entity。
    pub entity: Entity,
    /// ユーザが入力した原文字列。
    pub source: String,
    /// パース結果。失敗していれば `None`。数式セルのみ。
    pub stmt: Option<Stmt>,
    /// パース失敗の説明。
    pub parse_error: Option<String>,
    /// 種別。
    pub kind: CellKind,
}

impl Cell {
    /// 検証義務か。
    #[must_use]
    pub fn is_obligation(&self) -> bool {
        matches!(self.kind, CellKind::Obligation { .. })
    }

    /// このセル 1 つだけを見たときの信頼度。上流は見ない。
    ///
    /// 数式セルは [`Trust::Checked`] とする。**定義や計算は主張ではなく、
    /// 正しいかどうかを問えるものではない。** ここを `Unknown` にすると、
    /// セルを参照する全証明の実効信頼度が `Unknown` に落ちてしまう。
    #[must_use]
    pub fn own_trust(&self) -> Trust {
        match &self.kind {
            CellKind::Expression => Trust::Checked,
            CellKind::Obligation { assumed: true, .. } => Trust::Assumed,
            CellKind::Obligation { verdict, .. } => verdict
                .as_ref()
                .map(Verdict::trust)
                .unwrap_or(Trust::Unknown),
        }
    }

    /// 他の義務を検査するときに、文脈として差し込む行。
    ///
    /// **数式セルは何も出さない。** セルは mathrel の構文であって Lean の
    /// 構文ではなく、変換は段階 2 の課題である（`docs/検証層の設計.md` §6）。
    /// 依存は追跡するが、翻訳はしない。
    #[must_use]
    fn as_context(&self) -> Option<String> {
        match &self.kind {
            CellKind::Expression => None,
            CellKind::Obligation { obligation, .. } => Some(format!(
                "axiom {} : {}",
                obligation.name, obligation.statement
            )),
        }
    }
}

/// 実効信頼度が満点に届かないセルと、その原因。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WeakLink {
    /// 対象のセル。
    pub id: CellId,
    /// このセル 1 つだけを見たときの信頼度。
    pub own: Trust,
    /// 上流を畳み込んだ信頼度。**利用者に見せるべきはこちら。**
    pub effective: Trust,
    /// 実効信頼度を決めているセル。`id` と同じこともある。
    pub culprit: CellId,
    /// なぜ満点にならないのか。
    pub reason: String,
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
pub struct Workspace {
    kernel: Kernel,
    cells: Vec<Cell>,
    index: HashMap<Entity, usize>,
    values: HashMap<Entity, Value>,
    next_id: CellId,
    /// 検証義務を判定するもの。無ければ義務は `Unavailable` になる。
    verifier: Option<Rc<dyn Verifier>>,
}

impl core::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Workspace")
            .field("cells", &self.cells.len())
            .field("revision", &self.kernel.revision())
            .field(
                "verifier",
                &self.verifier.as_ref().map(|v| v.backend().id.clone()),
            )
            .finish()
    }
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
            verifier: None,
        }
    }

    /// 検証義務を判定する道具を差す。
    ///
    /// 設定しなければ、義務は [`Verdict::Unavailable`] のまま残る。
    /// **`Refuted` にはしない。** 道具がないことと、偽であることは別である。
    pub fn set_verifier(&mut self, verifier: Rc<dyn Verifier>) {
        self.verifier = Some(verifier);
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
            kind: CellKind::Expression,
        });
        id
    }

    /// 検証義務をセルとして足す。
    ///
    /// `uses` に挙げた名前は、その名前を定義しているセルへの依存になる。
    /// `cites` に挙げた名前は、他の義務への依存になる。**どちらも申告制で
    /// あり、命題をパースして推測することはしない**（カーネルの原則）。
    pub fn add_obligation(&mut self, obligation: Obligation) -> CellId {
        self.register_obligation(obligation, false)
    }

    /// 「これは既知である」と宣言して足す。
    ///
    /// 検証器へは渡さない。信頼度は必ず [`Trust::Assumed`] であり、これに
    /// 依存するものの実効信頼度もそこまで落ちる。数学の作業では `sorry` を
    /// 置いて先に進むのが普通であり、それを禁じると使い物にならない。
    pub fn add_assumption(&mut self, obligation: Obligation) -> CellId {
        self.register_obligation(obligation, true)
    }

    fn register_obligation(&mut self, obligation: Obligation, assumed: bool) -> CellId {
        let provides = vec![Capability::PropositionProved(
            self.kernel.intern(&obligation.name),
        )];
        let mut requires: Vec<Capability> = Vec::new();
        for name in &obligation.uses {
            requires.push(Capability::NameBound(self.kernel.intern(name)));
        }
        for name in &obligation.cites {
            requires.push(Capability::PropositionProved(self.kernel.intern(name)));
        }

        let source = format!("theorem {} : {}", obligation.name, obligation.statement);
        let entity = match self.kernel.add_item(ItemSpec {
            provides,
            requires,
            source: Some(source.clone()),
            ..Default::default()
        }) {
            Ok((entity, _report)) => entity,
            Err(_) => return 0,
        };

        let id = self.next_id;
        self.next_id += 1;
        self.index.insert(entity, self.cells.len());
        self.cells.push(Cell {
            id,
            entity,
            source,
            stmt: None,
            parse_error: None,
            kind: CellKind::Obligation {
                obligation: Box::new(obligation),
                assumed,
                verdict: None,
            },
        });
        id
    }

    /// 仮置きをやめ、検証の対象に戻す。
    ///
    /// 命題は変わらないので、これを引用している定理の指紋は変わらない。
    /// 検証が通れば、下流の実効信頼度だけが上がる。
    pub fn promote_assumption(&mut self, id: CellId) -> bool {
        let position = match self.cells.iter().position(|cell| cell.id == id) {
            Some(position) => position,
            None => return false,
        };
        let entity = self.cells[position].entity;
        match &mut self.cells[position].kind {
            CellKind::Obligation {
                assumed, verdict, ..
            } if *assumed => {
                *assumed = false;
                *verdict = None;
            }
            _ => return false,
        }
        self.kernel
            .change_value(entity, ValueUpdate::default())
            .is_ok()
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
                let (outcome, value, verdict) = self.compute(entity);
                match value {
                    Some(value) => {
                        self.values.insert(entity, value);
                    }
                    None => {
                        self.values.remove(&entity);
                    }
                }
                if let Some(verdict) = verdict {
                    if let Some(position) = self.index.get(&entity).copied() {
                        if let CellKind::Obligation { verdict: slot, .. } =
                            &mut self.cells[position].kind
                        {
                            *slot = Some(verdict);
                        }
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
    ///
    /// 返るのは (カーネルへの報告, 数式セルの値, 検証義務の判定)。
    fn compute(&self, entity: Entity) -> (EvalOutcome, Option<Value>, Option<Verdict>) {
        let cell = match self.cell_of(entity) {
            Some(cell) => cell,
            None => {
                return (
                    EvalOutcome::Failed {
                        reason: "対応するセルがありません".to_owned(),
                    },
                    None,
                    None,
                )
            }
        };

        if let CellKind::Obligation {
            obligation,
            assumed,
            ..
        } = &cell.kind
        {
            let (outcome, verdict) = self.verify_obligation(entity, obligation, *assumed);
            return (outcome, None, Some(verdict));
        }

        if let Some(error) = &cell.parse_error {
            return (
                EvalOutcome::Failed {
                    reason: error.clone(),
                },
                None,
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
                None,
            ),
            Stmt::FuncDef { params, body, .. } => {
                // 関数定義の指紋は、本体の形と、その本体が参照している上流の
                // 指紋から作る。自由変数 `c` が変われば f の指紋も変わり、
                // f を使うセルへ dirty が伝わる。
                let mut parts: Vec<String> =
                    vec!["func".to_owned(), params.join(","), body.canonical()];
                for upstream in self.kernel.dependencies(entity).unwrap_or_default() {
                    parts.push(self.upstream_fingerprint(upstream));
                }
                let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
                (
                    EvalOutcome::Value {
                        digest: digest_of(&refs),
                    },
                    None,
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
                        None,
                    ),
                    Err(error) => (
                        EvalOutcome::Failed {
                            reason: error.message,
                        },
                        None,
                        None,
                    ),
                }
            }
        }
    }

    /// 検証義務 1 件を判定する。カーネルの状態は変えない。
    ///
    /// 判定は指紋にも混ざる。証明本体は混ざらない（proof irrelevance。
    /// `Obligation::fingerprint` 参照）。
    fn verify_obligation(
        &self,
        entity: Entity,
        obligation: &Obligation,
        assumed: bool,
    ) -> (EvalOutcome, Verdict) {
        let (verdict, backend) = if assumed {
            // 検証器を呼ばない。呼んだところで中身は検査されないし、
            // 呼ぶと道具の版に判定が引きずられる。
            (
                Verdict::Proved {
                    trust: Trust::Assumed,
                },
                mathrel_verify::BackendId::new("assumption", "1"),
            )
        } else {
            match &self.verifier {
                Some(verifier) => {
                    let context = self.proof_context(entity);
                    (verifier.verify(obligation, &context), verifier.backend())
                }
                None => (
                    Verdict::Unavailable {
                        reason: "検証器が設定されていません".to_owned(),
                    },
                    mathrel_verify::BackendId::new("none", "0"),
                ),
            }
        };

        let upstream: Vec<mathrel_kernel::Digest> = self
            .kernel
            .dependencies(entity)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|provider| self.kernel.freshness(provider).ok())
            .filter_map(Freshness::digest)
            .collect();
        let digest = obligation.fingerprint(&backend, &verdict, &upstream);
        let outcome = verdict.to_outcome(digest);
        (outcome, verdict)
    }

    /// 検証に渡す文脈。上流の**義務だけ**を依存順に並べる。
    ///
    /// 引用した補題は `axiom` として宣言し、その証明は読み込まない。1 回の
    /// 検証が小さくなり、信頼度の合成は [`Self::effective_trust`] が行う
    /// （`docs/検証層の設計.md` ADR-006）。
    ///
    /// 数式セルは入らない。翻訳しないためである（同 §6 段階 2）。
    #[must_use]
    pub fn proof_context(&self, entity: Entity) -> Vec<String> {
        let mut ordered: Vec<Entity> = Vec::new();
        let mut visited: BTreeSet<Entity> = BTreeSet::new();
        for provider in self.kernel.dependencies(entity).unwrap_or_default() {
            self.collect_upstream(provider, &mut visited, &mut ordered);
        }
        ordered
            .into_iter()
            .filter_map(|upstream| self.cell_of(upstream))
            .filter_map(Cell::as_context)
            .collect()
    }

    /// 後行順で上流を集める。循環しても visited で止まる。
    fn collect_upstream(
        &self,
        entity: Entity,
        visited: &mut BTreeSet<Entity>,
        ordered: &mut Vec<Entity>,
    ) {
        if !visited.insert(entity) {
            return;
        }
        for provider in self.kernel.dependencies(entity).unwrap_or_default() {
            self.collect_upstream(provider, visited, ordered);
        }
        ordered.push(entity);
    }

    // ---------------------------------------------------------------
    // 信頼度
    // ---------------------------------------------------------------

    /// このセル 1 つだけを見たときの信頼度。
    #[must_use]
    pub fn own_trust(&self, id: CellId) -> Trust {
        self.cell(id).map(Cell::own_trust).unwrap_or(Trust::Unknown)
    }

    /// 上流をすべて畳み込んだ実効信頼度。
    ///
    /// `effective(c) = min( own(c), min over 上流 u の own(u) )`
    ///
    /// 検証器が完全に検査した定理でも、仮置きの補題に依存していれば
    /// `Assumed` にしかならない。「通ったから正しい」という誤った安心を、
    /// 構造的に防ぐ（`docs/検証層の設計.md` ADR-008）。
    #[must_use]
    pub fn effective_trust(&self, id: CellId) -> Trust {
        let cell = match self.cell(id) {
            Some(cell) => cell,
            None => return Trust::Unknown,
        };
        let mut trust = cell.own_trust();
        let mut visited: BTreeSet<Entity> = BTreeSet::new();
        let mut queue: Vec<Entity> = self.kernel.dependencies(cell.entity).unwrap_or_default();

        while let Some(current) = queue.pop() {
            if current == cell.entity || !visited.insert(current) {
                continue;
            }
            if let Some(upstream) = self.cell_of(current) {
                trust = trust.meet(upstream.own_trust());
            }
            queue.extend(self.kernel.dependencies(current).unwrap_or_default());
        }
        trust
    }

    /// 実効信頼度が満点でないものを、原因つきで挙げる。
    ///
    /// 「全体の信頼度は Assumed です」とだけ言われても、利用者は何をすれば
    /// よいか分からない。**どのセルが原因で、なぜそうなったか**まで出す。
    ///
    /// 信頼度が [`Trust::Checked`] に届かない原因は、次のいずれかである。
    ///
    /// | 原因 | 信頼度 |
    /// |---|---|
    /// | 「既知」として宣言しただけ（`add_assumption`） | `Assumed` |
    /// | 証明を書かず `sorry` を置いた | `Assumed` |
    /// | 検証器が無い / Lean が入っていない | `Unknown` |
    /// | 検証が通らなかった | `Unknown` |
    /// | 未解決の参照があって検証していない | `Unknown` |
    /// | 引用が循環していて検証していない | `Unknown` |
    /// | AI が提案しただけで誰も検査していない | `Suggested` |
    /// | 上流のいずれかが上記のどれか | 上流に引きずられる |
    #[must_use]
    pub fn weak_links(&self) -> Vec<WeakLink> {
        self.cells
            .iter()
            .filter(|cell| cell.is_obligation())
            .filter_map(|cell| {
                let effective = self.effective_trust(cell.id);
                if effective.is_checked() {
                    return None;
                }
                let culprit = self.culprit_for(cell, effective);
                Some(WeakLink {
                    id: cell.id,
                    own: cell.own_trust(),
                    effective,
                    culprit: culprit.id,
                    reason: self.reason_for(culprit),
                })
            })
            .collect()
    }

    /// 実効信頼度を決めている犯人を探す。自分自身のこともある。
    fn culprit_for<'a>(&'a self, cell: &'a Cell, effective: Trust) -> &'a Cell {
        if cell.own_trust() == effective {
            return cell;
        }
        let mut visited: BTreeSet<Entity> = BTreeSet::new();
        let mut queue: Vec<Entity> = self.kernel.dependencies(cell.entity).unwrap_or_default();
        while let Some(current) = queue.pop() {
            if !visited.insert(current) {
                continue;
            }
            if let Some(upstream) = self.cell_of(current) {
                if upstream.own_trust() == effective {
                    return upstream;
                }
            }
            queue.extend(self.kernel.dependencies(current).unwrap_or_default());
        }
        cell
    }

    /// なぜ満点にならないのかを一言で。
    fn reason_for(&self, cell: &Cell) -> String {
        match &cell.kind {
            CellKind::Expression => "数式セルが未評価です".to_owned(),
            CellKind::Obligation { assumed: true, .. } => {
                "「既知」として宣言しただけで、誰も検査していません".to_owned()
            }
            CellKind::Obligation { verdict, .. } => match verdict {
                Some(verdict) if verdict.is_unavailable() => format!(
                    "検証器がありません（{}）。偽だと言っているのではありません",
                    verdict.reason().unwrap_or("")
                ),
                Some(verdict) if !verdict.is_proved() => {
                    format!("検証が通りませんでした: {}", verdict.reason().unwrap_or(""))
                }
                Some(_) => "検査済みです".to_owned(),
                None => {
                    if self.kernel.in_cycle(cell.entity).unwrap_or(false) {
                        "引用が循環しているので検証していません".to_owned()
                    } else if self
                        .kernel
                        .resolution(cell.entity)
                        .map(|resolution| resolution.is_unresolved())
                        .unwrap_or(false)
                    {
                        "未解決の参照があるので検証していません".to_owned()
                    } else {
                        "まだ検証していません".to_owned()
                    }
                }
            },
        }
    }

    /// ワークスペース全体で最も弱い実効信頼度。UI の 1 行表示に使う。
    ///
    /// 検証義務が 1 つもなければ [`Trust::Checked`] を返す。主張がなければ
    /// 弱い環も存在しない。
    #[must_use]
    pub fn weakest_link(&self) -> Trust {
        self.cells
            .iter()
            .filter(|cell| cell.is_obligation())
            .map(|cell| self.effective_trust(cell.id))
            .min()
            .unwrap_or(Trust::Checked)
    }

    /// 検証義務の直近の判定。
    #[must_use]
    pub fn verdict(&self, id: CellId) -> Option<&Verdict> {
        match &self.cell(id)?.kind {
            CellKind::Obligation { verdict, .. } => verdict.as_ref(),
            CellKind::Expression => None,
        }
    }

    fn upstream_fingerprint(&self, entity: Entity) -> String {
        match self.kernel.freshness(entity) {
            Ok(Freshness::Clean { digest, .. }) => {
                digest.iter().map(|b| format!("{b:02x}")).collect()
            }
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
                        env.values
                            .entry(name.clone())
                            .or_insert_with(|| value.clone());
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
            // ワークスペース全体の信頼度は「最も弱い環」で決まる。UI はこれを
            // 1 行で出せる（`docs/検証層の設計.md` §4.4）。
            .string("weakestLink", self.weakest_link().label())
            // 満点でないものを、原因つきで出す。UI はこれを読んで警告を出す。
            .array(
                "weakLinks",
                self.weak_links()
                    .iter()
                    .map(|weak| {
                        Object::new()
                            .number("id", weak.id as f64)
                            .string("own", weak.own.label())
                            .string("effective", weak.effective.label())
                            .number("culprit", weak.culprit as f64)
                            .string("reason", &weak.reason)
                            .build()
                    })
                    .collect(),
            )
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
        let value_type = self
            .value(cell.id)
            .map(|value| value.type_name().to_owned());
        let failure = self
            .kernel
            .failure(entity)
            .ok()
            .flatten()
            .map(str::to_owned)
            .or_else(|| cell.parse_error.clone());

        let (kind, name) = match &cell.kind {
            CellKind::Obligation { obligation, .. } => {
                ("obligation", Some(obligation.name.clone()))
            }
            CellKind::Expression => (
                cell.stmt.as_ref().map_or("invalid", Stmt::kind_str),
                cell.stmt
                    .as_ref()
                    .and_then(Stmt::declared_name)
                    .map(str::to_owned),
            ),
        };
        let assumed = matches!(cell.kind, CellKind::Obligation { assumed: true, .. });

        Object::new()
            .number("id", cell.id as f64)
            .string("source", &cell.source)
            .string("kind", kind)
            // 信頼度は 2 つ出す。`trust` はこのセル 1 つの判定、
            // `effectiveTrust` は上流を畳み込んだもの。**表示に使うべきは
            // 後者である。** 前者だけを出すと、仮置きに依存した証明が
            // 検査済みに見える（`docs/検証層の設計.md` §6.0(d)）。
            .string("trust", cell.own_trust().label())
            .string("effectiveTrust", self.effective_trust(cell.id).label())
            .boolean("assumed", assumed)
            .optional_string("name", name.as_deref())
            .string("freshness", freshness)
            .string("resolution", resolution.kind_str())
            .boolean("inCycle", self.kernel.in_cycle(entity).unwrap_or(false))
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

/// `名前 : 命題 [uses a,b] [cites c,d]` を [`Obligation`] にする。
///
/// `uses` / `cites` は省略できる。命題の中に同じ語が現れる場合と区別する
/// ため、空白で区切られた語として**末尾にあるもの**だけを見る。
///
/// 名前と命題の両方が要る。欠けていれば `None`。
///
/// ```
/// use mathrel_host::parse_obligation_line;
///
/// let obligation = parse_obligation_line("f_nonneg : 0 <= f t uses f cites base")
///     .expect("parse");
/// assert_eq!(obligation.name, "f_nonneg");
/// assert_eq!(obligation.statement, "0 <= f t");
/// assert_eq!(obligation.uses, vec!["f".to_owned()]);
/// assert_eq!(obligation.cites, vec!["base".to_owned()]);
/// ```
#[must_use]
pub fn parse_obligation_line(line: &str) -> Option<Obligation> {
    let (name, rest) = line.split_once(':')?;
    let name = name.trim();
    let (statement, uses, cites) = split_dependencies(rest.trim());
    if name.is_empty() || statement.is_empty() {
        return None;
    }
    let use_refs: Vec<&str> = uses.iter().map(String::as_str).collect();
    let cite_refs: Vec<&str> = cites.iter().map(String::as_str).collect();
    Some(
        Obligation::new(name, &statement)
            .using(&use_refs)
            .citing(&cite_refs),
    )
}

/// `<命題> uses a,b cites c,d` を分解する。
///
/// `uses` / `cites` は省略できる。命題の中に現れる場合と区別するため、
/// 空白で区切られた語として末尾にあるものだけを見る。
fn split_dependencies(rest: &str) -> (String, Vec<String>, Vec<String>) {
    let mut statement = rest.trim().to_owned();
    let mut uses = Vec::new();
    let mut cites = Vec::new();

    // 後ろから順に剥がす。`uses` が先に書かれても `cites` が先でも動く。
    loop {
        let lower = statement.to_lowercase();
        let at_uses = lower.rfind(" uses ");
        let at_cites = lower.rfind(" cites ");
        let at = match (at_uses, at_cites) {
            (Some(u), Some(c)) => u.max(c),
            (Some(u), None) => u,
            (None, Some(c)) => c,
            (None, None) => break,
        };
        let (head, tail) = statement.split_at(at);
        let tail = tail.trim();
        let (keyword, names) = match tail.split_once(char::is_whitespace) {
            Some(parts) => parts,
            None => break,
        };
        let parsed: Vec<String> = names
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        if keyword.eq_ignore_ascii_case("uses") {
            uses.splice(0..0, parsed);
        } else {
            cites.splice(0..0, parsed);
        }
        statement = head.trim_end().to_owned();
    }

    (statement, uses, cites)
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
