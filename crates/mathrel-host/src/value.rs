//! 値と、その指紋。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// 評価結果の値。
///
/// v0.1 のホストはスカラーとベクトルだけを扱う。行列・複素数・記号式は
/// 企画書 P4 以降のバックエンド接続で扱う。
#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    /// 実数。
    Scalar(f64),
    /// 実ベクトル。
    Vector(Vec<f64>),
}

impl Value {
    /// 型を表す短い名前。
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Scalar(_) => "Real",
            Value::Vector(_) => "Vector",
        }
    }

    /// スカラーとして取り出す。
    #[must_use]
    pub fn as_scalar(&self) -> Option<f64> {
        match self {
            Value::Scalar(value) => Some(*value),
            Value::Vector(_) => None,
        }
    }

    /// 表示用の文字列。
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Value::Scalar(value) => render_scalar(*value),
            Value::Vector(items) => {
                let inner: Vec<String> = items.iter().map(|item| render_scalar(*item)).collect();
                format!("[{}]", inner.join(", "))
            }
        }
    }
}

fn render_scalar(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "∞" } else { "-∞" }.to_owned();
    }
    if value == value.trunc() && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    let rendered = format!("{value:.10}");
    let trimmed = rendered.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_owned()
}

/// 32 バイトの指紋を作る。
///
/// カーネルは指紋の中身を解釈しない。等値比較しかしないため、暗号学的な
/// 強度は不要である。ここでは標準ハッシャを 4 レーン走らせて 256 bit を作る。
/// 衝突しても「変わったのに Clean のまま」になるだけだが、値が偶然衝突する
/// 確率は実用上無視できる。
#[must_use]
pub fn digest_of(parts: &[&str]) -> [u8; 32] {
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

/// 値の指紋。
///
/// 浮動小数は `to_bits` で比較する。`0.0` と `-0.0` は別の指紋になるが、
/// 早期カットオフが 1 回余分に外れるだけで、健全性は損なわれない。
#[must_use]
pub fn digest_of_value(value: &Value) -> [u8; 32] {
    match value {
        Value::Scalar(scalar) => digest_of(&["scalar", &scalar.to_bits().to_string()]),
        Value::Vector(items) => {
            let mut parts: Vec<String> = vec!["vector".to_owned()];
            parts.extend(items.iter().map(|item| item.to_bits().to_string()));
            let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
            digest_of(&refs)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_values_share_a_digest() {
        assert_eq!(
            digest_of_value(&Value::Scalar(2.0)),
            digest_of_value(&Value::Scalar(2.0))
        );
    }

    #[test]
    fn different_values_differ() {
        assert_ne!(
            digest_of_value(&Value::Scalar(2.0)),
            digest_of_value(&Value::Scalar(3.0))
        );
        assert_ne!(
            digest_of_value(&Value::Scalar(1.0)),
            digest_of_value(&Value::Vector(vec![1.0]))
        );
    }

    #[test]
    fn rendering_is_readable() {
        assert_eq!(Value::Scalar(5.0).render(), "5");
        assert_eq!(Value::Scalar(0.5).render(), "0.5");
        assert_eq!(Value::Vector(vec![1.0, 2.0]).render(), "[1, 2]");
    }
}
