//! 信頼度の束。
//!
//! 「Lean が通した」と「AI がそう言った」と「人間が既知と宣言した」を
//! 同じ扱いにはできない。かといって前者以外を禁止すると、数学の作業に
//! 使えない道具になる（`sorry` を置いて先に進むのは普通のことである）。
//!
//! 本設計は禁止も放置もせず、**信頼度を第一級にして依存に沿って伝播させる**。
//! 詳細は `docs/検証層の設計.md` §4 と ADR-008。

/// 主張がどれだけ確からしいか。
///
/// 順序は `Unknown < Suggested < Assumed < Checked` である。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum Trust {
    /// 何も分かっていない。未検証、または検証が失敗した。
    #[default]
    Unknown,
    /// AI が主張しただけ。誰も検査していない。
    Suggested,
    /// 人間が「既知」と宣言した。中身は検査していない。
    Assumed,
    /// 証明支援系が検査した。
    Checked,
}

impl Trust {
    /// 下限（meet）。2 つの信頼度のうち弱い方。
    ///
    /// 依存の連鎖に沿ってこれを畳み込むと実効信頼度になる。
    #[must_use]
    pub fn meet(self, other: Self) -> Self {
        if self <= other {
            self
        } else {
            other
        }
    }

    /// 表示用の短い名前。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Trust::Unknown => "Unknown",
            Trust::Suggested => "Suggested",
            Trust::Assumed => "Assumed",
            Trust::Checked => "Checked",
        }
    }

    /// 証明支援系が検査したものか。
    #[must_use]
    pub fn is_checked(self) -> bool {
        matches!(self, Trust::Checked)
    }
}

impl core::fmt::Display for Trust {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_is_the_strongest_and_unknown_the_weakest() {
        assert!(Trust::Checked > Trust::Assumed);
        assert!(Trust::Assumed > Trust::Suggested);
        assert!(Trust::Suggested > Trust::Unknown);
        assert_eq!(Trust::default(), Trust::Unknown);
    }

    #[test]
    fn meet_takes_the_weaker_side() {
        assert_eq!(Trust::Checked.meet(Trust::Suggested), Trust::Suggested);
        assert_eq!(Trust::Suggested.meet(Trust::Checked), Trust::Suggested);
        assert_eq!(Trust::Assumed.meet(Trust::Assumed), Trust::Assumed);
        assert_eq!(Trust::Checked.meet(Trust::Unknown), Trust::Unknown);
    }

    /// 束であること。実効信頼度をどの順で畳み込んでも同じ答えになる。
    #[test]
    fn meet_is_a_semilattice() {
        let all = [
            Trust::Unknown,
            Trust::Suggested,
            Trust::Assumed,
            Trust::Checked,
        ];
        for a in all {
            assert_eq!(a.meet(a), a, "冪等");
            for b in all {
                assert_eq!(a.meet(b), b.meet(a), "可換");
                for c in all {
                    assert_eq!(a.meet(b).meet(c), a.meet(b.meet(c)), "結合");
                }
            }
        }
    }
}
