//! エンティティの状態モデル。
//!
//! 状態は直交する 3 軸で持つ。単一の enum に潰さない。
//!
//! | 軸 | 型 |
//! |---|---|
//! | 参照解決 | [`Resolution`] |
//! | 鮮度 | [`Freshness`] |
//! | 循環 | `in_cycle: bool` |

use crate::entity::Entity;
use crate::relation::Capability;

/// ワークスペース全体の単調増加カウンタ。変更操作ごとに +1 される。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Revision(pub u64);

impl Revision {
    /// 次のリビジョン。
    #[must_use]
    pub fn next(self) -> Self {
        Revision(self.0.wrapping_add(1))
    }
}

impl core::fmt::Display for Revision {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// 評価結果の指紋。カーネルは中身を解釈しない。
///
/// 生成はホストの責務である（要件定義書 ADR-004）。カーネルは等値比較のみ行う。
pub type Digest = [u8; 32];

/// 参照解決の状態。
///
/// `Unresolved` と `Ambiguous` は同時に起こり得る。要件定義書 §13.1 の
/// 未確定事項に対し、本実装は「`Unresolved` を優先し、曖昧情報も同時に保持する」
/// 案を採った。
///
/// SPEC-GAP: §13.1 — 未解決が 1 件でもあれば `Unresolved` を返し、
/// 曖昧情報は `ambiguous` フィールドに載せる。情報が落ちないことを優先した。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Resolution {
    /// すべての requires に一意な provider がある。
    #[default]
    Resolved,
    /// provider が見つからない requires がある。
    Unresolved {
        /// provider が存在しない Capability。
        missing: Vec<Capability>,
        /// 同時に曖昧でもある Capability とその provider 群。
        ambiguous: Vec<(Capability, Vec<Entity>)>,
    },
    /// すべて解決したが、provider が複数ある Capability がある。
    Ambiguous {
        /// 複数 provider を持つ Capability とその provider 群。
        conflicts: Vec<(Capability, Vec<Entity>)>,
    },
}

impl Resolution {
    /// 完全に解決済みか。
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        matches!(self, Resolution::Resolved)
    }

    /// 未解決の Capability を持つか。
    #[must_use]
    pub fn is_unresolved(&self) -> bool {
        matches!(self, Resolution::Unresolved { .. })
    }

    /// 曖昧な Capability を持つか。`Unresolved` に同居している場合も含む。
    #[must_use]
    pub fn is_ambiguous(&self) -> bool {
        match self {
            Resolution::Ambiguous { .. } => true,
            Resolution::Unresolved { ambiguous, .. } => !ambiguous.is_empty(),
            Resolution::Resolved => false,
        }
    }

    /// provider が存在しない Capability。
    #[must_use]
    pub fn missing(&self) -> &[Capability] {
        match self {
            Resolution::Unresolved { missing, .. } => missing,
            _ => &[],
        }
    }

    /// 複数 provider を持つ Capability。
    #[must_use]
    pub fn conflicts(&self) -> &[(Capability, Vec<Entity>)] {
        match self {
            Resolution::Ambiguous { conflicts } => conflicts,
            Resolution::Unresolved { ambiguous, .. } => ambiguous,
            Resolution::Resolved => &[],
        }
    }

    /// 表示・スナップショット用の短い名前。
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Resolution::Resolved => "Resolved",
            Resolution::Unresolved { .. } => "Unresolved",
            Resolution::Ambiguous { .. } => "Ambiguous",
        }
    }
}

/// 計算結果の鮮度。
///
/// `MaybeDirty` を `Dirty` と分けているのは早期カットオフのためである。
/// 上流が変わっても値が変わらなければ下流は再計算しなくてよい。
/// この区別がないと、可能性ベースの伝播が生む偽陽性がそのまま残る
/// （Autexier et al., arXiv:1105.2392）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Freshness {
    /// 一度も評価されていない。
    #[default]
    NeverEvaluated,
    /// 評価済みで、上流にも変更がない。
    Clean {
        /// 評価が確定したリビジョン。
        at_revision: Revision,
        /// 評価結果の指紋。
        digest: Digest,
    },
    /// 上流が変わった可能性があるが、実際に値が変わったかは未確認。
    MaybeDirty,
    /// 再計算が必要であることが確定している。
    Dirty,
}

impl Freshness {
    /// 再計算が必要か（`next_batch` の候補になるか）。
    #[must_use]
    pub fn needs_evaluation(self) -> bool {
        !matches!(self, Freshness::Clean { .. })
    }

    /// 上流として「落ち着いている」とみなせるか。
    ///
    /// 下流をスケジュールしてよいのは、全上流が `Clean` のときだけである。
    #[must_use]
    pub fn is_settled(self) -> bool {
        matches!(self, Freshness::Clean { .. })
    }

    /// 表示・スナップショット用の短い名前。
    #[must_use]
    pub fn kind_str(self) -> &'static str {
        match self {
            Freshness::NeverEvaluated => "NeverEvaluated",
            Freshness::Clean { .. } => "Clean",
            Freshness::MaybeDirty => "MaybeDirty",
            Freshness::Dirty => "Dirty",
        }
    }

    /// `Clean` なら指紋を返す。
    #[must_use]
    pub fn digest(self) -> Option<Digest> {
        match self {
            Freshness::Clean { digest, .. } => Some(digest),
            _ => None,
        }
    }
}

/// 鮮度と、直近の Clean 時点の記録。
///
/// `last_clean` は早期カットオフのために要る。`MaybeDirty` から `Clean` へ
/// 戻すとき、前回の指紋を復元する必要があるため。
#[derive(Clone, Debug, Default)]
pub struct EvalState {
    /// 現在の鮮度。
    pub freshness: Freshness,
    /// 直近に `Clean` だったときの (リビジョン, 指紋)。
    pub last_clean: Option<(Revision, Digest)>,
    /// 直近の評価が失敗した場合の理由。
    pub failure: Option<String>,
}

impl EvalState {
    /// 新規エンティティの初期状態。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}
