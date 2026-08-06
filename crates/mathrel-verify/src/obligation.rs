//! 検証義務 — 「これを確かめてほしい」という 1 件。

use crate::fingerprint::{digest_of, to_hex};
use crate::verdict::Verdict;
use mathrel_kernel::Digest;

/// 検証器の同一性。指紋に混ぜるので、版が変われば全証明が再検査になる。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BackendId {
    /// 検証器の名前（`lean4`, `trivial`, ...）。
    pub id: String,
    /// 版。Mathlib のリビジョンなど、判定を左右するものはすべてここに含める。
    pub version: String,
}

impl BackendId {
    /// 名前と版から作る。
    #[must_use]
    pub fn new(id: &str, version: &str) -> Self {
        Self {
            id: id.to_owned(),
            version: version.to_owned(),
        }
    }
}

/// 1 件の検証義務。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Obligation {
    /// 命題の名前。他の義務からはこの名前で引用される。
    pub name: String,
    /// 命題そのもの。**指紋に入る**。
    pub statement: String,
    /// 証明。**指紋に入らない**（§ [`Obligation::fingerprint`]）。
    pub proof: Option<String>,
    /// 引用する命題の名前。
    pub cites: Vec<String>,
    /// 参照する定義の名前。
    pub uses: Vec<String>,
}

impl Obligation {
    /// 名前と命題から作る。
    #[must_use]
    pub fn new(name: &str, statement: &str) -> Self {
        Self {
            name: name.to_owned(),
            statement: statement.to_owned(),
            ..Default::default()
        }
    }

    /// 証明を付ける。
    #[must_use]
    pub fn with_proof(mut self, proof: &str) -> Self {
        self.proof = Some(proof.to_owned());
        self
    }

    /// 引用する命題を足す。
    #[must_use]
    pub fn citing(mut self, names: &[&str]) -> Self {
        self.cites
            .extend(names.iter().map(|name| (*name).to_owned()));
        self
    }

    /// 参照する定義を足す。
    #[must_use]
    pub fn using(mut self, names: &[&str]) -> Self {
        self.uses
            .extend(names.iter().map(|name| (*name).to_owned()));
        self
    }

    /// この義務の指紋。
    ///
    /// ## 何を入れ、何を入れないか
    ///
    /// 入れるもの: 命題・検証器の同一性・判定の種別と信頼度・**上流の指紋**。
    ///
    /// 入れないもの: **証明本体**。定理の利用者が依存しているのは命題であって
    /// 証明ではない（proof irrelevance）。証明を書き直しても命題が同じなら、
    /// その定理を引用している他の定理を再検査する必要はない。証明を指紋に
    /// 入れてしまうと、tactic を 1 行整形しただけで下流が全部再検査になる。
    ///
    /// 「証明していないのに通ったことになる」経路がないのは、判定の種別を
    /// 指紋に入れているからではなく、証明できなければ `EvalOutcome::Failed`
    /// になって `Clean` に到達しないからである（`Verdict::to_outcome`）。
    #[must_use]
    pub fn fingerprint(
        &self,
        backend: &BackendId,
        verdict: &Verdict,
        upstream: &[Digest],
    ) -> Digest {
        let mut parts: Vec<String> = vec![
            "obligation".to_owned(),
            self.statement.clone(),
            backend.id.clone(),
            backend.version.clone(),
            verdict.kind_str().to_owned(),
            verdict.trust().label().to_owned(),
        ];
        // 上流は昇順に正規化してから混ぜる。順序が揺れると指紋が揺れる。
        let mut hexes: Vec<String> = upstream.iter().map(to_hex).collect();
        hexes.sort();
        parts.extend(hexes);

        let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
        digest_of(&refs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::Trust;

    fn backend() -> BackendId {
        BackendId::new("lean4", "4.8.0+mathlib@abc123")
    }

    fn proved() -> Verdict {
        Verdict::Proved {
            trust: Trust::Checked,
        }
    }

    #[test]
    fn the_same_obligation_gives_the_same_fingerprint() {
        let obligation = Obligation::new("thm", "∀ t, 0 ≤ f t");
        assert_eq!(
            obligation.fingerprint(&backend(), &proved(), &[[1u8; 32]]),
            obligation.fingerprint(&backend(), &proved(), &[[1u8; 32]])
        );
    }

    /// 証明を書き直しても、その定理を引用する側は再検査しなくてよい。
    #[test]
    fn rewriting_the_proof_does_not_change_the_fingerprint() {
        let first = Obligation::new("thm", "∀ t, 0 ≤ f t").with_proof("by simp");
        let second = Obligation::new("thm", "∀ t, 0 ≤ f t").with_proof("by positivity");
        assert_eq!(
            first.fingerprint(&backend(), &proved(), &[]),
            second.fingerprint(&backend(), &proved(), &[]),
            "proof irrelevance: 命題が同じなら下流に影響しない"
        );
    }

    #[test]
    fn changing_the_statement_changes_the_fingerprint() {
        assert_ne!(
            Obligation::new("thm", "∀ t, 0 ≤ f t").fingerprint(&backend(), &proved(), &[]),
            Obligation::new("thm", "∀ t, 0 < f t").fingerprint(&backend(), &proved(), &[])
        );
    }

    /// Lean や Mathlib を上げたら、全証明が再検査される。高価だが正しい。
    #[test]
    fn changing_the_backend_version_changes_the_fingerprint() {
        let obligation = Obligation::new("thm", "P");
        assert_ne!(
            obligation.fingerprint(&BackendId::new("lean4", "4.8.0"), &proved(), &[]),
            obligation.fingerprint(&BackendId::new("lean4", "4.9.0"), &proved(), &[])
        );
        assert_ne!(
            obligation.fingerprint(&BackendId::new("lean4", "4.8.0"), &proved(), &[]),
            obligation.fingerprint(&BackendId::new("trivial", "4.8.0"), &proved(), &[])
        );
    }

    #[test]
    fn changing_an_upstream_digest_changes_the_fingerprint() {
        let obligation = Obligation::new("thm", "P");
        assert_ne!(
            obligation.fingerprint(&backend(), &proved(), &[[1u8; 32]]),
            obligation.fingerprint(&backend(), &proved(), &[[2u8; 32]])
        );
    }

    /// 上流の並び順が揺れても指紋は揺れない。
    #[test]
    fn upstream_order_does_not_matter() {
        let obligation = Obligation::new("thm", "P");
        assert_eq!(
            obligation.fingerprint(&backend(), &proved(), &[[1u8; 32], [2u8; 32]]),
            obligation.fingerprint(&backend(), &proved(), &[[2u8; 32], [1u8; 32]])
        );
    }

    /// 同じ命題でも、信頼度が違えば別の状態である。
    #[test]
    fn trust_is_part_of_the_fingerprint() {
        let obligation = Obligation::new("thm", "P");
        assert_ne!(
            obligation.fingerprint(
                &backend(),
                &Verdict::Proved {
                    trust: Trust::Checked
                },
                &[]
            ),
            obligation.fingerprint(
                &backend(),
                &Verdict::Proved {
                    trust: Trust::Assumed
                },
                &[]
            )
        );
    }
}
