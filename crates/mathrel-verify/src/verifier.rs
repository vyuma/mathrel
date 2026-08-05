//! 検証器の境界と、依存のない実装 2 つ。

use crate::obligation::{BackendId, Obligation};
use crate::trust::Trust;
use crate::verdict::Verdict;

/// 「正しいかどうかを判定するもの」。
///
/// この trait を実装するものだけが [`Verdict`] を作れる。AI は実装しない
/// （`docs/検証層の設計.md` ADR-007）。
pub trait Verifier {
    /// 検証器の同一性。指紋に混ざる。
    fn backend(&self) -> BackendId;

    /// 1 件の義務を判定する。
    ///
    /// `context` には上流の定義や補題の本文が、依存順で入っている。検証器は
    /// これを使って完結した入力を組み立てる。
    fn verify(&self, obligation: &Obligation, context: &[String]) -> Verdict;
}

// NOTE: かつて `AssumptionVerifier`（何でも `Assumed` で通す検証器）を置いて
// いたが、削除した。「人間が既知と宣言した」は**義務そのものの属性**であって、
// どの道具で検査するかとは無関係である。検証器の差し替えで表現すると、
// 検証器を切り替えるたびに仮置きの記録が失われ、道具の版に判定が引きずられる。
// 仮置きは `ProofSpace::add_assumption` を使うこと。

/// 構文的に自明なものだけを通す検証器。
///
/// 証明支援系を入れずに、機構が回っていることを確かめるためのもの。
/// `x = x` の形（両辺が文字単位で一致）だけを `Checked` として通す。
/// それ以外は判定しない（`Refuted` ではなく [`Verdict::Unknown`]）。
/// **通せないことと、偽であることは別**だからである。
#[derive(Clone, Debug, Default)]
pub struct TrivialVerifier;

impl TrivialVerifier {
    /// 命題が反射律の形か。
    fn is_reflexive(statement: &str) -> bool {
        let mut sides = statement.splitn(2, '=');
        match (sides.next(), sides.next()) {
            (Some(left), Some(right)) => {
                let left = left.trim();
                !left.is_empty() && left == right.trim()
            }
            _ => false,
        }
    }
}

impl Verifier for TrivialVerifier {
    fn backend(&self) -> BackendId {
        BackendId::new("trivial", "1")
    }

    fn verify(&self, obligation: &Obligation, _context: &[String]) -> Verdict {
        if Self::is_reflexive(&obligation.statement) {
            Verdict::Proved {
                trust: Trust::Checked,
            }
        } else {
            Verdict::Unknown {
                reason: format!(
                    "trivial 検証器は反射律しか扱えません: {}",
                    obligation.statement
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trivial_verifier_proves_reflexivity() {
        let verdict = TrivialVerifier.verify(&Obligation::new("refl", "f x = f x"), &[]);
        assert_eq!(
            verdict,
            Verdict::Proved {
                trust: Trust::Checked
            }
        );
    }

    /// 手に負えないものを「偽」と言ってはならない。
    #[test]
    fn the_trivial_verifier_says_unknown_not_refuted() {
        let verdict = TrivialVerifier.verify(&Obligation::new("hard", "∀ t, 0 ≤ f t"), &[]);
        assert_eq!(verdict.kind_str(), "Unknown");
        assert!(!verdict.is_proved());
    }

    #[test]
    fn an_empty_left_hand_side_is_not_reflexive() {
        assert!(!TrivialVerifier::is_reflexive(" = "));
        assert!(!TrivialVerifier::is_reflexive("a = b"));
        assert!(TrivialVerifier::is_reflexive("a = a"));
    }
}
