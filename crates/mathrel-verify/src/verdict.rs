//! 検証器の判定。

use crate::trust::Trust;
use mathrel_kernel::{Digest, EvalOutcome};

/// 検証器が 1 つの義務について出した結論。
///
/// `Refuted` と `Unavailable` を分けているのが要点である。**道具がないことと、
/// 偽であることは別**である。Lean が入っていない環境で全証明が「反証された」
/// と表示されたら、この道具は信用を失う。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// 証明された。どの程度の信頼度でかは `trust` が持つ。
    Proved {
        /// この判定の信頼度。検証器が自分で名乗る。
        trust: Trust,
    },
    /// 証明が通らなかった。
    Refuted {
        /// 検証器の出力（Lean のエラーなど）。AI への差し戻しに使う。
        reason: String,
    },
    /// 判定できなかった。時間切れなど。
    Unknown {
        /// 理由。
        reason: String,
    },
    /// 検証器が使えなかった。Lean が入っていない、など。
    Unavailable {
        /// 理由。
        reason: String,
    },
}

impl Verdict {
    /// この判定が主張する信頼度。証明されていなければ `Unknown`。
    #[must_use]
    pub fn trust(&self) -> Trust {
        match self {
            Verdict::Proved { trust } => *trust,
            _ => Trust::Unknown,
        }
    }

    /// 証明されたか。
    #[must_use]
    pub fn is_proved(&self) -> bool {
        matches!(self, Verdict::Proved { .. })
    }

    /// 検証器が動かなかっただけか。
    ///
    /// これが真のとき、利用者に見せるべきは「証明が壊れた」ではなく
    /// 「検証器を用意してください」である。
    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Verdict::Unavailable { .. })
    }

    /// 表示・指紋用の短い名前。
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Verdict::Proved { .. } => "Proved",
            Verdict::Refuted { .. } => "Refuted",
            Verdict::Unknown { .. } => "Unknown",
            Verdict::Unavailable { .. } => "Unavailable",
        }
    }

    /// 検証器の出力。証明できた場合は `None`。
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Verdict::Proved { .. } => None,
            Verdict::Refuted { reason }
            | Verdict::Unknown { reason }
            | Verdict::Unavailable { reason } => Some(reason),
        }
    }

    /// カーネルへ報告する形に変える。
    ///
    /// これがあるおかげで、検証は評価とまったく同じループに載る
    /// （`docs/検証層の設計.md` ADR-006）。
    #[must_use]
    pub fn to_outcome(&self, digest: Digest) -> EvalOutcome {
        match self {
            Verdict::Proved { .. } => EvalOutcome::Value { digest },
            Verdict::Refuted { reason }
            | Verdict::Unknown { reason }
            | Verdict::Unavailable { reason } => EvalOutcome::Failed {
                reason: reason.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_proved_carries_trust() {
        assert_eq!(
            Verdict::Proved {
                trust: Trust::Checked
            }
            .trust(),
            Trust::Checked
        );
        assert_eq!(
            Verdict::Refuted {
                reason: "型が合わない".to_owned()
            }
            .trust(),
            Trust::Unknown
        );
    }

    /// 道具がないことと、偽であることは別である。
    #[test]
    fn unavailable_is_not_refuted() {
        let unavailable = Verdict::Unavailable {
            reason: "lean が見つかりません".to_owned(),
        };
        let refuted = Verdict::Refuted {
            reason: "lean が見つかりません".to_owned(),
        };
        assert_ne!(unavailable, refuted);
        assert!(unavailable.is_unavailable());
        assert!(!refuted.is_unavailable());
        assert!(!unavailable.is_proved());
    }

    #[test]
    fn proved_becomes_a_value_and_everything_else_fails() {
        let digest = [7u8; 32];
        assert_eq!(
            Verdict::Proved {
                trust: Trust::Checked
            }
            .to_outcome(digest),
            EvalOutcome::Value { digest }
        );
        assert_eq!(
            Verdict::Refuted {
                reason: "だめ".to_owned()
            }
            .to_outcome(digest),
            EvalOutcome::Failed {
                reason: "だめ".to_owned()
            }
        );
    }
}
