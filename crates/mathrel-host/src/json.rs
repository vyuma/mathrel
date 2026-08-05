//! 最小の JSON 出力ヘルパ。
//!
//! wasm バイナリを小さく保つため、シリアライズライブラリに依存しない。
//! 読み込みは行わない（ホストは JSON を出力するだけ）。

/// JSON 文字列リテラルとしてエスケープする（引用符を含む）。
#[must_use]
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 数値。非有限値は `null` にする（JSON に NaN / Infinity はない）。
#[must_use]
pub fn number(value: f64) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "null".to_owned()
    }
}

/// オブジェクトを組み立てる。
#[derive(Default, Debug)]
pub struct Object {
    fields: Vec<String>,
}

impl Object {
    /// 空のオブジェクト。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 生の JSON 値をそのまま入れる。
    #[must_use]
    pub fn raw(mut self, key: &str, value: impl Into<String>) -> Self {
        self.fields.push(format!("{}:{}", quote(key), value.into()));
        self
    }

    /// 文字列フィールド。
    #[must_use]
    pub fn string(self, key: &str, value: &str) -> Self {
        let quoted = quote(value);
        self.raw(key, quoted)
    }

    /// 省略可能な文字列フィールド。`None` は `null`。
    #[must_use]
    pub fn optional_string(self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(text) => self.string(key, text),
            None => self.raw(key, "null"),
        }
    }

    /// 数値フィールド。
    #[must_use]
    pub fn number(self, key: &str, value: f64) -> Self {
        let rendered = number(value);
        self.raw(key, rendered)
    }

    /// 真偽値フィールド。
    #[must_use]
    pub fn boolean(self, key: &str, value: bool) -> Self {
        self.raw(key, if value { "true" } else { "false" })
    }

    /// 文字列の配列。
    #[must_use]
    pub fn string_array(self, key: &str, values: &[String]) -> Self {
        let items: Vec<String> = values.iter().map(|value| quote(value)).collect();
        self.raw(key, format!("[{}]", items.join(",")))
    }

    /// 生の JSON 値の配列。
    #[must_use]
    pub fn array(self, key: &str, values: Vec<String>) -> Self {
        self.raw(key, format!("[{}]", values.join(",")))
    }

    /// 組み立てた JSON を返す。
    #[must_use]
    pub fn build(self) -> String {
        format!("{{{}}}", self.fields.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_special_characters() {
        assert_eq!(quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn builds_an_object() {
        let json = Object::new()
            .string("name", "x")
            .number("value", 2.0)
            .boolean("dirty", false)
            .build();
        assert_eq!(json, r#"{"name":"x","value":2,"dirty":false}"#);
    }

    #[test]
    fn non_finite_numbers_become_null() {
        assert_eq!(number(f64::NAN), "null");
        assert_eq!(number(f64::INFINITY), "null");
    }
}
