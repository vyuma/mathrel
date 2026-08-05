//! 指紋の生成。
//!
//! カーネルは指紋の中身を解釈せず、等値比較しかしない（ADR-004）。したがって
//! 暗号学的な強度は不要で、エンティティごとに構成が違っても構わない。
//! `mathrel_host::digest_of` と同じ 4 レーン構成だが、一致している必要はない。

use mathrel_kernel::Digest;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// 文字列の並びから 32 バイトの指紋を作る。
///
/// 区切りバイトを挟むので、`["ab", "c"]` と `["a", "bc"]` は別の指紋になる。
#[must_use]
pub fn digest_of(parts: &[&str]) -> Digest {
    let mut output = [0u8; 32];
    for (lane, chunk) in output.chunks_mut(8).enumerate() {
        let mut hasher = DefaultHasher::new();
        (lane as u64).hash(&mut hasher);
        0x9E37_79B9_7F4A_7C15u64.hash(&mut hasher);
        for part in parts {
            part.hash(&mut hasher);
            0xFFu8.hash(&mut hasher);
        }
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    output
}

/// 指紋を 16 進文字列にする。指紋どうしを混ぜるときに使う。
#[must_use]
pub fn to_hex(digest: &Digest) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_input_gives_the_same_digest() {
        assert_eq!(digest_of(&["a", "b"]), digest_of(&["a", "b"]));
    }

    #[test]
    fn the_separator_prevents_run_together_collisions() {
        assert_ne!(digest_of(&["ab", "c"]), digest_of(&["a", "bc"]));
    }

    #[test]
    fn hex_is_stable_and_full_width() {
        assert_eq!(to_hex(&[0u8; 32]).len(), 64);
        assert!(to_hex(&[255u8; 32]).starts_with("ffff"));
    }
}
