//! 検証空間 — 定義と証明を、カーネルの 1 つのグラフに載せる。
//!
//! `y = f(x)` が古くなるのと、`定理: f は非負` が古くなるのは同じ現象である。
//! したがってグラフは 1 つでよい（`docs/検証層の設計.md` ADR-006）。
//!
//! ## 各義務は、引用した補題を `axiom` として検査する
//!
//! `定理 B` が `補題 A` を引用するとき、B の検査では A を `axiom` として
//! 宣言し、A 自身の証明は読み込まない。理由は 2 つある。
//!
//! 1. **速い**。1 回の Lean 起動が小さくなる。連鎖が深くても、各段は
//!    自分の分だけを検査する。
//! 2. **信頼度が正しく合成される**。A が `sorry` で仮置きされていれば、
//!    A の信頼度は `Assumed` になり、[`ProofSpace::effective_trust`] が B を
//!    そこまで引き下げる。B の Lean 検査が通ったことと、B が信用できることは
//!    別であり、後者は信頼度の畳み込みが答える。
//!
//! `axiom` を使うと偽の命題を仮定して何でも証明できてしまうが、A 自身も
//! 別途検査されるので合成としては健全である。循環（A が B を、B が A を
//! 引用する）はカーネルが検出して両方を評価対象から外す。

use crate::obligation::{BackendId, Obligation};
use crate::trust::Trust;
use crate::verdict::Verdict;
use crate::verifier::Verifier;
use mathrel_kernel::{
    Capability, Digest, Entity, EvalOutcome, Freshness, ItemSpec, Kernel, ValueUpdate,
};
use std::collections::{BTreeSet, HashMap};

use crate::fingerprint::digest_of;

/// 検証空間に載るものの種別。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EntryKind {
    /// 定義。主張ではないので、信頼度の下限にはならない。
    Definition {
        /// 本文（Lean のソースなど）。
        body: String,
    },
    /// 検証義務。
    Obligation(Box<Obligation>),
}

/// 検証空間の 1 件。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// 対応するカーネル Entity。
    pub entity: Entity,
    /// 名前。
    pub name: String,
    /// 種別。
    pub kind: EntryKind,
    /// 直近の判定。未検証なら `None`。
    pub verdict: Option<Verdict>,
    /// 人間が「既知」と宣言したもの。検証器へは渡さない。
    ///
    /// 検証器を差し替えることで仮置きを表現してはならない。仮置きかどうかは
    /// **義務そのものの属性**であり、どの道具で検査するかとは無関係である
    /// （`docs/検証層の設計.md` §4.2）。
    pub assumed: bool,
}

impl Entry {
    /// この 1 件だけを見たときの信頼度。上流は見ない。
    ///
    /// 定義は [`Trust::Checked`] とする。定義は「正しいかどうか」を問える
    /// ものではなく、与えられたものだからである。ここを `Unknown` にすると、
    /// 定義に依存する全証明の実効信頼度が `Unknown` に落ちてしまう。
    #[must_use]
    pub fn own_trust(&self) -> Trust {
        match &self.kind {
            EntryKind::Definition { body } if smuggles_an_assumption(body) => Trust::Assumed,
            EntryKind::Definition { .. } => Trust::Checked,
            EntryKind::Obligation(_) if self.assumed => Trust::Assumed,
            EntryKind::Obligation(_) => self
                .verdict
                .as_ref()
                .map(Verdict::trust)
                .unwrap_or(Trust::Unknown),
        }
    }

    /// 他の義務を検査するときに、文脈として差し込む行。
    #[must_use]
    pub fn as_context(&self) -> String {
        match &self.kind {
            EntryKind::Definition { body } => body.clone(),
            EntryKind::Obligation(obligation) => {
                format!("axiom {} : {}", obligation.name, obligation.statement)
            }
        }
    }
}

/// 検証の統計。
///
/// `checked` と `assumed` を分けているのは、消費側が「成立 N 件」とだけ
/// 表示して、仮置きを検査済みに見せてしまうのを防ぐためである。
/// **さらに、ここに出るのはどれも 1 件ごとの判定であって実効信頼度ではない。**
/// 表示に使うなら [`ProofSpace::weakest_link`] を併記すること。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VerifyStats {
    /// 証明支援系が検査して通った件数。
    pub checked: usize,
    /// 仮置き、または `sorry` 込みで通った件数。**検査されていない。**
    pub assumed: usize,
    /// 証明できなかった件数。
    pub failed: usize,
    /// 検証器が動かなかった件数。
    pub unavailable: usize,
    /// 実際に検証器へ渡した義務の名前（順番つき）。
    pub verified: Vec<String>,
}

impl VerifyStats {
    /// 何らかの形で通った件数。**内訳を見ずにこれだけを表示しないこと。**
    #[must_use]
    pub fn proved(&self) -> usize {
        self.checked + self.assumed
    }
}

/// 定義と検証義務を 1 つのグラフで管理する。
#[derive(Debug)]
pub struct ProofSpace {
    kernel: Kernel,
    entries: Vec<Entry>,
    index: HashMap<Entity, usize>,
    /// 直近に使った検証器。変わったら全義務を再検査する。
    last_backend: Option<BackendId>,
}

impl Default for ProofSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl ProofSpace {
    /// 空の検証空間。
    #[must_use]
    pub fn new() -> Self {
        Self {
            kernel: Kernel::new(),
            entries: Vec::new(),
            index: HashMap::new(),
            last_backend: None,
        }
    }

    /// 内側のカーネル。
    #[must_use]
    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    /// 登録されているもの一覧。
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// 名前で引く。
    #[must_use]
    pub fn entry_named(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    // ---------------------------------------------------------------
    // 登録
    // ---------------------------------------------------------------

    /// 定義を登録する。他の定義には依存しない。
    pub fn add_definition(&mut self, name: &str, body: &str) -> Entity {
        self.add_definition_using(name, body, &[])
    }

    /// 他の定義を参照する定義を登録する。
    ///
    /// `def f (t : ℝ) : ℝ := t^2 + c` は `c` に依存する。**この依存は
    /// 呼び出し側が明示的に渡す。** 本文をパースして推測することはしない
    /// （カーネルの原則。`mathrel_kernel` の crate ドキュメント参照）。
    pub fn add_definition_using(&mut self, name: &str, body: &str, uses: &[&str]) -> Entity {
        let provides = vec![Capability::NameBound(self.kernel.intern(name))];
        let requires: Vec<Capability> = uses
            .iter()
            .map(|used| Capability::NameBound(self.kernel.intern(used)))
            .collect();
        let entity = self
            .kernel
            .add_item(ItemSpec {
                provides,
                requires,
                source: Some(body.to_owned()),
                ..Default::default()
            })
            .expect("add_item は失敗しない")
            .0;
        self.push(Entry {
            entity,
            name: name.to_owned(),
            kind: EntryKind::Definition {
                body: body.to_owned(),
            },
            verdict: None,
            assumed: false,
        });
        entity
    }

    /// 定義を書き換える。依存する証明は再検査の対象になる。
    pub fn update_definition(&mut self, entity: Entity, body: &str) -> bool {
        let position = match self.index.get(&entity) {
            Some(position) => *position,
            None => return false,
        };
        if self
            .kernel
            .change_value(
                entity,
                ValueUpdate {
                    source: Some(body.to_owned()),
                    ..Default::default()
                },
            )
            .is_err()
        {
            return false;
        }
        self.entries[position].kind = EntryKind::Definition {
            body: body.to_owned(),
        };
        true
    }

    /// 検証義務を登録する。
    ///
    /// `uses` は定義への参照、`cites` は他の命題への参照になる。どちらも
    /// カーネルの `requires` として登録される。**依存の推測はしない**
    /// （カーネルの原則。`lib.rs` 参照）。
    pub fn add_obligation(&mut self, obligation: Obligation) -> Entity {
        self.register(obligation, false)
    }

    /// 「これは既知である」と宣言して登録する。
    ///
    /// 検証器へは渡さない。信頼度は必ず [`Trust::Assumed`] であり、これに
    /// 依存するものの実効信頼度もそこまで落ちる。数学の作業では `sorry` を
    /// 置いて先に進むのが普通であり、それを禁じると使い物にならない
    /// （`docs/検証層の設計.md` §4.2）。
    pub fn add_assumption(&mut self, obligation: Obligation) -> Entity {
        self.register(obligation, true)
    }

    fn register(&mut self, obligation: Obligation, assumed: bool) -> Entity {
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

        let name = obligation.name.clone();
        let source = format!("theorem {} : {}", name, obligation.statement);
        let entity = self
            .kernel
            .add_item(ItemSpec {
                provides,
                requires,
                source: Some(source),
                ..Default::default()
            })
            .expect("add_item は失敗しない")
            .0;
        self.push(Entry {
            entity,
            name,
            kind: EntryKind::Obligation(Box::new(obligation)),
            verdict: None,
            assumed,
        });
        entity
    }

    /// 義務の証明を差し替える。
    ///
    /// 命題は変えないので、**この義務を引用している定理は再検査されない**
    /// （proof irrelevance。`Obligation::fingerprint` 参照）。
    pub fn set_proof(&mut self, entity: Entity, proof: &str) -> bool {
        let position = match self.index.get(&entity) {
            Some(position) => *position,
            None => return false,
        };
        let obligation = match &mut self.entries[position].kind {
            EntryKind::Obligation(obligation) => obligation,
            EntryKind::Definition { .. } => return false,
        };
        obligation.proof = Some(proof.to_owned());
        self.kernel
            .change_value(entity, ValueUpdate::default())
            .is_ok()
    }

    // ---------------------------------------------------------------
    // 検証
    // ---------------------------------------------------------------

    /// 再検査が必要なものを、依存順にすべて検証する。
    ///
    /// **何を検査すべきかを決めるのはカーネルである。** ここは
    /// `next_batch()` が返したものだけを回す。早期カットオフが効いていれば、
    /// 定義を触っても指紋が変わらなかった証明は 1 つも呼ばれない。
    pub fn verify_all(&mut self, verifier: &dyn Verifier) -> VerifyStats {
        // 検証器が前回と違えば、指紋の材料が変わっている。しかしカーネルは
        // それを知らない。`next_batch` は「古いもの」しか返さないので、
        // 全部 Clean のまま何も起きない。
        //
        // 「Lean の版を上げたら全証明が再検査される」は、ここで明示的に
        // 古くしない限り成立しない。指紋に版を混ぜただけでは足りない。
        let backend = verifier.backend();
        if self.last_backend.as_ref() != Some(&backend) {
            self.invalidate_all_obligations();
            self.last_backend = Some(backend);
        }

        let mut stats = VerifyStats::default();
        loop {
            let batch = self.kernel.next_batch();
            if batch.is_empty() {
                break;
            }
            let mut progressed = false;
            for entity in batch {
                let (outcome, verdict) = self.compute(entity, verifier);
                if let Some(position) = self.index.get(&entity).copied() {
                    if let Some(verdict) = verdict.clone() {
                        stats.verified.push(self.entries[position].name.clone());
                        match &verdict {
                            Verdict::Proved { trust } if trust.is_checked() => {
                                stats.checked += 1;
                            }
                            Verdict::Proved { .. } => stats.assumed += 1,
                            Verdict::Unavailable { .. } => stats.unavailable += 1,
                            _ => stats.failed += 1,
                        }
                    }
                    self.entries[position].verdict = verdict;
                }
                if self.kernel.commit_evaluation(entity, outcome).is_ok() {
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        stats
    }

    /// 仮置きをやめ、検証の対象に戻す。
    ///
    /// 命題は変わらないので、これを引用している定理の指紋は変わらない。
    /// 検証が通れば、下流の実効信頼度だけが上がる。
    pub fn promote_assumption(&mut self, entity: Entity) -> bool {
        let position = match self.index.get(&entity) {
            Some(position) => *position,
            None => return false,
        };
        if !self.entries[position].assumed {
            return false;
        }
        self.entries[position].assumed = false;
        self.entries[position].verdict = None;
        self.kernel
            .change_value(entity, ValueUpdate::default())
            .is_ok()
    }

    /// すべての義務を再検査の対象に戻す。
    fn invalidate_all_obligations(&mut self) {
        let targets: Vec<Entity> = self
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Obligation(_)) && !entry.assumed)
            .map(|entry| entry.entity)
            .collect();
        for entity in targets {
            let _ = self.kernel.change_value(entity, ValueUpdate::default());
        }
    }

    /// 検証に失敗した義務を、もう一度検証の対象に戻す。戻した件数を返す。
    ///
    /// カーネルは、失敗した項目を「入力が変わるまで」再スケジュールしない
    /// （同じ入力なら同じ結果になるため）。しかし検証には**入力が変わって
    /// いないのに結果が変わる**場合がある。Lean を入れた、Mathlib を更新した、
    /// 時間切れの上限を伸ばした、といった環境側の変化である。
    ///
    /// そのときはこれを呼ぶ。検証器の版が変わっていれば指紋も変わるので、
    /// 早期カットオフが誤って下流を据え置くことはない。
    pub fn retry_failed(&mut self) -> usize {
        let targets: Vec<Entity> = self
            .entries
            .iter()
            .filter(|entry| {
                matches!(entry.kind, EntryKind::Obligation(_))
                    && entry
                        .verdict
                        .as_ref()
                        .map(|verdict| !verdict.is_proved())
                        .unwrap_or(false)
            })
            .map(|entry| entry.entity)
            .collect();

        let mut retried = 0;
        for entity in targets {
            if self
                .kernel
                .change_value(entity, ValueUpdate::default())
                .is_ok()
            {
                retried += 1;
            }
        }
        retried
    }

    /// 1 件を検証する。カーネルの状態は変えない。
    fn compute(
        &self,
        entity: Entity,
        verifier: &dyn Verifier,
    ) -> (EvalOutcome, Option<Verdict>) {
        let entry = match self.index.get(&entity).and_then(|p| self.entries.get(*p)) {
            Some(entry) => entry,
            None => {
                return (
                    EvalOutcome::Failed {
                        reason: "対応する項目がありません".to_owned(),
                    },
                    None,
                )
            }
        };

        match &entry.kind {
            EntryKind::Definition { body } => (
                EvalOutcome::Value {
                    digest: self.definition_digest(entity, &entry.name, body),
                },
                None,
            ),
            EntryKind::Obligation(obligation) if entry.assumed => {
                // 検証器を呼ばない。呼んだところで中身は検査されないし、
                // 呼ぶと道具の版に判定が引きずられる。
                let verdict = Verdict::Proved {
                    trust: Trust::Assumed,
                };
                let backend = BackendId::new("assumption", "1");
                let digest =
                    obligation.fingerprint(&backend, &verdict, &self.upstream_digests(entity));
                (verdict.to_outcome(digest), Some(verdict))
            }
            EntryKind::Obligation(obligation) => {
                let context = self.context_for(entity);
                let verdict = verifier.verify(obligation, &context);
                let digest =
                    obligation.fingerprint(&verifier.backend(), &verdict, &self.upstream_digests(entity));
                (verdict.to_outcome(digest), Some(verdict))
            }
        }
    }

    /// 定義の指紋。
    ///
    /// **本文だけでは足りない。上流の指紋を混ぜる。** `def f (t) := t + c` の
    /// 本文は `c` を書き換えても変わらないが、`f` の意味は変わる。混ぜないと
    /// `f` の指紋が据え置きになり、早期カットオフが `f` に依存する証明を
    /// `Clean` のまま残してしまう。それは最適化の効きすぎではなく、健全性の穴
    /// である（`docs/検証層の設計.md` §3.1）。
    fn definition_digest(&self, entity: Entity, name: &str, body: &str) -> Digest {
        let mut parts: Vec<String> =
            vec!["definition".to_owned(), name.to_owned(), body.to_owned()];
        let mut upstream: Vec<String> = self
            .upstream_digests(entity)
            .iter()
            .map(crate::fingerprint::to_hex)
            .collect();
        upstream.sort();
        parts.extend(upstream);

        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        digest_of(&refs)
    }

    /// 検証に渡す文脈。上流を依存順（上流が先）に並べる。
    #[must_use]
    pub fn context_for(&self, entity: Entity) -> Vec<String> {
        let mut ordered: Vec<Entity> = Vec::new();
        let mut visited: BTreeSet<Entity> = BTreeSet::new();
        for provider in self.kernel.dependencies(entity).unwrap_or_default() {
            self.collect_upstream(provider, &mut visited, &mut ordered);
        }
        ordered
            .into_iter()
            .filter_map(|upstream| self.index.get(&upstream))
            .filter_map(|position| self.entries.get(*position))
            .map(Entry::as_context)
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

    fn upstream_digests(&self, entity: Entity) -> Vec<Digest> {
        self.kernel
            .dependencies(entity)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|provider| self.kernel.freshness(provider).ok())
            .filter_map(Freshness::digest)
            .collect()
    }

    // ---------------------------------------------------------------
    // 信頼度
    // ---------------------------------------------------------------

    /// この 1 件だけを見たときの信頼度。
    #[must_use]
    pub fn own_trust(&self, entity: Entity) -> Trust {
        self.index
            .get(&entity)
            .and_then(|position| self.entries.get(*position))
            .map(Entry::own_trust)
            .unwrap_or(Trust::Unknown)
    }

    /// 上流をすべて畳み込んだ実効信頼度。
    ///
    /// `effective(O) = min( own(O), min over 上流 u の own(u) )`
    ///
    /// Lean が完全に検査した定理でも、AI が仮置きした補題に依存していれば
    /// `Suggested` にしかならない。「Lean が通ったから正しい」という誤った
    /// 安心を、構造的に防ぐ（ADR-008）。
    ///
    /// 依存グラフはカーネルが既に持っているので、ここで作るものは何もない。
    #[must_use]
    pub fn effective_trust(&self, entity: Entity) -> Trust {
        let mut trust = self.own_trust(entity);
        let mut visited: BTreeSet<Entity> = BTreeSet::new();
        let mut queue: Vec<Entity> = self.kernel.dependencies(entity).unwrap_or_default();

        while let Some(current) = queue.pop() {
            if current == entity || !visited.insert(current) {
                continue;
            }
            trust = trust.meet(self.own_trust(current));
            queue.extend(self.kernel.dependencies(current).unwrap_or_default());
        }
        trust
    }

    /// 空間全体で最も弱い実効信頼度。UI の 1 行表示に使う。
    ///
    /// 定義しかない空間では [`Trust::Checked`] を返す。主張が 1 つもなければ
    /// 弱い環も存在しない。
    #[must_use]
    pub fn weakest_link(&self) -> Trust {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Obligation(_)))
            .map(|entry| self.effective_trust(entry.entity))
            .min()
            .unwrap_or(Trust::Checked)
    }

    /// 実効信頼度が `threshold` に満たない義務の名前。
    #[must_use]
    pub fn below(&self, threshold: Trust) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.kind, EntryKind::Obligation(_)))
            .filter(|entry| self.effective_trust(entry.entity) < threshold)
            .map(|entry| entry.name.clone())
            .collect()
    }

    // ---------------------------------------------------------------
    // 内部
    // ---------------------------------------------------------------

    fn push(&mut self, entry: Entry) {
        self.index.insert(entry.entity, self.entries.len());
        self.entries.push(entry);
    }
}

/// 定義の本文が、検査されていない仮定を持ち込んでいるか。
///
/// 定義は検証器へ渡らず、無条件に [`Trust::Checked`] として扱われる。定義は
/// 主張ではないからである。ところが本文は任意のソースなので、そこに `axiom`
/// や `sorry` を書けば、**信頼度追跡を素通りして仮定を持ち込める**。
///
/// これは構文の一致による粗い検出であり、取りこぼしはある。ただし外す方向は
/// 常に「信頼度を下げる」側なので、誤検出しても健全性は損なわれない。
/// 本質的な対策は、定義も検証器に通して公理依存を確認することである（P3）。
fn smuggles_an_assumption(body: &str) -> bool {
    body.lines().map(str::trim_start).any(|line| {
        line.starts_with("axiom ")
            || line.starts_with("axiom\t")
            || line.starts_with("sorry")
            || line.contains(":= sorry")
    })
}

/// 検証器の同一性を、記録用に文字列化する。
#[must_use]
pub fn backend_label(backend: &BackendId) -> String {
    format!("{}@{}", backend.id, backend.version)
}
