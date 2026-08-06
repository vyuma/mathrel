//! MathLive の `<math-field>` を扱うための最小の道具。
//!
//! MathLive は Web Component である。`wasm-bindgen` の型付きバインディングを
//! 書き起こす価値はないので、必要な 2 つ（値の読み書き）だけを
//! [`js_sys::Reflect`] で触る。
//!
//! **読み込めなかった場合も動くようにしてある。** `<math-field>` が未定義の
//! ままなら、ブラウザはそれを未知の要素として扱う。そのとき画面は素の
//! テキスト入力に落ちる（[`available`] を見て切り替える）。オフラインでも
//! ワークスペースそのものは使える。

use wasm_bindgen::JsCast;

/// MathLive が読み込まれているか。
///
/// カスタム要素が登録されているかで判定する。CDN が落ちていても、この関数が
/// `false` を返すだけで画面は動く。
#[must_use]
pub fn available() -> bool {
    let window = match web_sys::window() {
        Some(window) => window,
        None => return false,
    };
    let registry = match js_sys::Reflect::get(&window, &"customElements".into()) {
        Ok(registry) if !registry.is_undefined() && !registry.is_null() => registry,
        _ => return false,
    };
    let get = match js_sys::Reflect::get(&registry, &"get".into()) {
        Ok(get) => get,
        Err(_) => return false,
    };
    let function = match get.dyn_into::<js_sys::Function>() {
        Ok(function) => function,
        Err(_) => return false,
    };
    match function.call1(&registry, &"math-field".into()) {
        Ok(result) => !result.is_undefined(),
        Err(_) => false,
    }
}

/// 要素の `value` を読む。
///
/// `<math-field>` は LaTeX を、`<input>` は素の文字列を返す。どちらも
/// `value` プロパティなので、同じ道で取れる。
#[must_use]
pub fn value_of(target: &web_sys::EventTarget) -> Option<String> {
    js_sys::Reflect::get(target, &"value".into())
        .ok()
        .and_then(|value| value.as_string())
}

/// イベントから `value` を読む。
#[must_use]
pub fn value_from(event: &web_sys::Event) -> Option<String> {
    event.target().as_ref().and_then(value_of)
}
