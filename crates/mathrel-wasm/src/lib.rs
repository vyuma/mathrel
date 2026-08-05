//! # mathrel-wasm
//!
//! [`mathrel_host::Workspace`] を素の C ABI で公開する。
//!
//! ## wasm-bindgen を使わない理由
//!
//! 企画書 §9 の依存方針（本体依存を増やさない）に従う。公開するのは
//! 「バイト列を受け取り、JSON のバイト列を返す」だけなので、生成器を挟む
//! 必要がない。wasm バイナリも小さく保てる。
//!
//! ## 呼び出し規約
//!
//! 戻り値にポインタと長さを詰め込む方式は使わない。wasm32 ではポインタが
//! 32 bit に収まるが、ネイティブでは収まらず、**同じコードをネイティブで
//! テストできなくなる**。代わりに、結果はスレッドローカルの緩衝領域に置き、
//! 長さを返す。ポインタは [`mr_result_ptr`] で取る。
//!
//! ```text
//! // JS 側
//! const handle = mr_workspace_new();
//!
//! const bytes = new TextEncoder().encode("x = 2");
//! const ptr = mr_alloc(bytes.length);
//! new Uint8Array(memory.buffer, ptr, bytes.length).set(bytes);
//! const id = mr_add_cell(handle, ptr, bytes.length);
//! mr_free(ptr, bytes.length);
//!
//! mr_evaluate(handle);
//! const len = mr_snapshot(handle);
//! const json = new TextDecoder().decode(
//!     new Uint8Array(memory.buffer, mr_result_ptr(), len)
//! );
//!
//! mr_workspace_free(handle);
//! ```
//!
//! 文字列は UTF-8 とみなす。不正なバイト列は置換文字になり、パースエラーの
//! あるセルとして残る。落ちることはない。

#![warn(missing_docs)]
#![warn(clippy::all)]

use mathrel_host::Workspace;
use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    /// 生きているワークスペース。
    static WORKSPACES: RefCell<HashMap<u32, Workspace>> = RefCell::new(HashMap::new());
    /// 次に配る取っ手。
    static NEXT_HANDLE: RefCell<u32> = const { RefCell::new(1) };
    /// 直近の結果。[`mr_result_ptr`] が指す先。
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// 操作が成功した。
pub const MR_OK: i32 = 0;
/// 取っ手が無効だった。
pub const MR_BAD_HANDLE: i32 = -1;
/// 指定されたセルがない。
pub const MR_BAD_CELL: i32 = -2;

// ---------------------------------------------------------------------
// 記憶領域
// ---------------------------------------------------------------------

/// `len` バイトの領域を確保する。呼び出し側が [`mr_free`] で返すこと。
///
/// # Safety
///
/// 返ったポインタは、`len` バイトぶんだけ書き込んでよい。
#[no_mangle]
pub extern "C" fn mr_alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len);
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// [`mr_alloc`] で取った領域を返す。
///
/// # Safety
///
/// `ptr` は [`mr_alloc`] が返したもので、`len` はそのときと同じ値でなければ
/// ならない。同じ領域を 2 回渡してはならない。
#[no_mangle]
pub unsafe extern "C" fn mr_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    drop(Vec::from_raw_parts(ptr, 0, len));
}

/// 直近の結果が置かれている場所。
///
/// 次に結果を返す関数を呼ぶまで有効である。
#[no_mangle]
pub extern "C" fn mr_result_ptr() -> *const u8 {
    RESULT.with(|result| result.borrow().as_ptr())
}

/// 直近の結果の長さ。
#[no_mangle]
pub extern "C" fn mr_result_len() -> usize {
    RESULT.with(|result| result.borrow().len())
}

// ---------------------------------------------------------------------
// ワークスペース
// ---------------------------------------------------------------------

/// 新しいワークスペースを作り、取っ手を返す。
#[no_mangle]
pub extern "C" fn mr_workspace_new() -> u32 {
    let handle = NEXT_HANDLE.with(|next| {
        let mut next = next.borrow_mut();
        let handle = *next;
        *next = next.wrapping_add(1).max(1);
        handle
    });
    WORKSPACES.with(|workspaces| {
        workspaces.borrow_mut().insert(handle, Workspace::new());
    });
    handle
}

/// ワークスペースを捨てる。
#[no_mangle]
pub extern "C" fn mr_workspace_free(handle: u32) -> i32 {
    WORKSPACES.with(|workspaces| {
        if workspaces.borrow_mut().remove(&handle).is_some() {
            MR_OK
        } else {
            MR_BAD_HANDLE
        }
    })
}

/// セルを足す。戻り値はセル番号。取っ手が無効なら 0。
///
/// # Safety
///
/// `ptr` から `len` バイトが読める領域を指していなければならない。
#[no_mangle]
pub unsafe extern "C" fn mr_add_cell(handle: u32, ptr: *const u8, len: usize) -> u64 {
    let source = read_utf8(ptr, len);
    with_workspace(handle, |workspace| workspace.add_cell(&source)).unwrap_or(0)
}

/// セルを書き換える。
///
/// # Safety
///
/// `ptr` から `len` バイトが読める領域を指していなければならない。
#[no_mangle]
pub unsafe extern "C" fn mr_update_cell(
    handle: u32,
    id: u64,
    ptr: *const u8,
    len: usize,
) -> i32 {
    let source = read_utf8(ptr, len);
    with_workspace(handle, |workspace| {
        if workspace.update_cell(id, &source).is_ok() {
            MR_OK
        } else {
            MR_BAD_CELL
        }
    })
    .unwrap_or(MR_BAD_HANDLE)
}

/// セルを消す。
#[no_mangle]
pub extern "C" fn mr_remove_cell(handle: u32, id: u64) -> i32 {
    with_workspace(handle, |workspace| {
        if workspace.remove_cell(id).is_ok() {
            MR_OK
        } else {
            MR_BAD_CELL
        }
    })
    .unwrap_or(MR_BAD_HANDLE)
}

/// 再計算が必要なものを評価する。
///
/// 統計を JSON で結果領域に置き、その長さを返す。取っ手が無効なら 0。
#[no_mangle]
pub extern "C" fn mr_evaluate(handle: u32) -> usize {
    let json = with_workspace(handle, |workspace| {
        let stats = workspace.evaluate();
        Workspace::stats_json(&stats)
    });
    match json {
        Some(json) => set_result(json),
        None => set_result(String::new()),
    }
}

/// 現在の状態を JSON で結果領域に置き、その長さを返す。
#[no_mangle]
pub extern "C" fn mr_snapshot(handle: u32) -> usize {
    let json = with_workspace(handle, |workspace| workspace.to_json());
    match json {
        Some(json) => set_result(json),
        None => set_result(String::new()),
    }
}

// ---------------------------------------------------------------------
// 内部
// ---------------------------------------------------------------------

fn with_workspace<T>(handle: u32, action: impl FnOnce(&mut Workspace) -> T) -> Option<T> {
    WORKSPACES.with(|workspaces| {
        workspaces
            .borrow_mut()
            .get_mut(&handle)
            .map(action)
    })
}

fn set_result(text: String) -> usize {
    RESULT.with(|result| {
        let mut result = result.borrow_mut();
        *result = text.into_bytes();
        result.len()
    })
}

/// # Safety
///
/// `ptr` から `len` バイトが読めること。`len` が 0 なら `ptr` は見ない。
unsafe fn read_utf8(ptr: *const u8, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文字列を wasm 側の作法どおりに渡す。確保・書き込み・呼び出し・解放。
    fn add(handle: u32, source: &str) -> u64 {
        let bytes = source.as_bytes();
        let ptr = mr_alloc(bytes.len());
        // SAFETY: たった今 bytes.len() バイトを確保した。
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let id = mr_add_cell(handle, ptr, bytes.len());
            mr_free(ptr, bytes.len());
            id
        }
    }

    fn update(handle: u32, id: u64, source: &str) -> i32 {
        let bytes = source.as_bytes();
        let ptr = mr_alloc(bytes.len());
        // SAFETY: 同上。
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let status = mr_update_cell(handle, id, ptr, bytes.len());
            mr_free(ptr, bytes.len());
            status
        }
    }

    /// 結果領域を JS 側と同じ手順で読む。
    fn result(len: usize) -> String {
        let ptr = mr_result_ptr();
        // SAFETY: 直前の呼び出しが len バイトを置いた。
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn a_workspace_round_trips_through_the_abi() {
        let handle = mr_workspace_new();
        assert_ne!(handle, 0);

        assert_eq!(add(handle, "x = 2"), 1);
        assert_eq!(add(handle, "y = x + 1"), 2);

        let stats = result(mr_evaluate(handle));
        assert!(stats.contains("\"evaluated\":2"), "{stats}");

        let snapshot = result(mr_snapshot(handle));
        assert!(snapshot.contains("\"revision\""), "{snapshot}");
        assert!(snapshot.contains("\"value\":\"3\""), "{snapshot}");

        assert_eq!(mr_workspace_free(handle), MR_OK);
    }

    /// 上流を変えると、下流だけが再計算される。ABI 越しでも同じ。
    #[test]
    fn updating_upstream_recomputes_only_downstream() {
        let handle = mr_workspace_new();
        add(handle, "x = 2");
        add(handle, "y = x + 1");
        add(handle, "z = y + 1");
        mr_evaluate(handle);

        assert_eq!(update(handle, 1, "x = 10"), MR_OK);
        let stats = result(mr_evaluate(handle));
        assert!(stats.contains("\"order\":[1,2,3]"), "{stats}");

        let snapshot = result(mr_snapshot(handle));
        assert!(snapshot.contains("\"value\":\"12\""), "{snapshot}");
        mr_workspace_free(handle);
    }

    #[test]
    fn removing_a_cell_works_through_the_abi() {
        let handle = mr_workspace_new();
        add(handle, "x = 2");
        mr_evaluate(handle);
        assert_eq!(mr_remove_cell(handle, 1), MR_OK);
        assert_eq!(mr_remove_cell(handle, 1), MR_BAD_CELL);
        mr_workspace_free(handle);
    }

    #[test]
    fn handles_are_independent() {
        let first = mr_workspace_new();
        let second = mr_workspace_new();
        assert_ne!(first, second);

        add(first, "x = 1");
        mr_evaluate(first);
        let snapshot = result(mr_snapshot(second));
        assert!(snapshot.contains("\"cells\":[]"), "{snapshot}");

        mr_workspace_free(first);
        mr_workspace_free(second);
    }

    /// 無効な取っ手でもパニックしない。
    #[test]
    fn a_bad_handle_is_an_error_not_a_panic() {
        let missing = 99_999;
        assert_eq!(mr_workspace_free(missing), MR_BAD_HANDLE);
        assert_eq!(mr_remove_cell(missing, 1), MR_BAD_HANDLE);
        assert_eq!(update(missing, 1, "x = 1"), MR_BAD_HANDLE);
        assert_eq!(add(missing, "x = 1"), 0);
        assert_eq!(mr_evaluate(missing), 0);
        assert_eq!(mr_snapshot(missing), 0);
    }

    /// 空ポインタや長さ 0 でも落ちない。
    #[test]
    fn null_input_is_treated_as_an_empty_string() {
        let handle = mr_workspace_new();
        // SAFETY: 長さ 0 なのでポインタは読まれない。
        let id = unsafe { mr_add_cell(handle, std::ptr::null(), 0) };
        assert_ne!(id, 0, "空のセルとして登録される");
        mr_evaluate(handle);
        mr_workspace_free(handle);
    }

    /// UTF-8 でないバイト列を渡しても落ちない。
    #[test]
    fn invalid_utf8_does_not_crash() {
        let handle = mr_workspace_new();
        let bytes = [0xffu8, 0xfe, b'=', b'1'];
        let ptr = mr_alloc(bytes.len());
        // SAFETY: たった今 bytes.len() バイトを確保した。
        let id = unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let id = mr_add_cell(handle, ptr, bytes.len());
            mr_free(ptr, bytes.len());
            id
        };
        assert_ne!(id, 0);
        mr_evaluate(handle);
        let snapshot = result(mr_snapshot(handle));
        assert!(snapshot.contains("\"error\""), "{snapshot}");
        mr_workspace_free(handle);
    }

    #[test]
    fn result_len_agrees_with_the_last_call() {
        let handle = mr_workspace_new();
        add(handle, "x = 2");
        let len = mr_snapshot(handle);
        assert_eq!(len, mr_result_len());
        mr_workspace_free(handle);
    }
}
