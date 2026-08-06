// mathrel の wasm ABI を包む薄い層。
//
// ここは DOM を触らない。ブラウザでも Node でも同じものが動く。UI から
// 切り離してあるのは、**ブラウザなしでテストできるようにするため**である
// （`web/test.mjs`）。
//
// ABI の作法は `crates/mathrel-wasm/src/lib.rs` の冒頭にある。要点は 2 つ。
//
//   1. 文字列を渡すときは mr_alloc → 書き込み → 呼ぶ → mr_free
//   2. 返ってくる文字列は、長さが戻り値で、場所は mr_result_ptr()
//
// ポインタと長さを 1 つの整数に詰める方式は使っていない。wasm32 では収まるが
// ネイティブでは収まらず、同じコードをネイティブでテストできなくなるため。

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * wasm を読み込んでワークスペースを 1 つ作る。
 *
 * @param {BufferSource|Response|Promise<Response>} source - .wasm のバイト列か fetch の結果
 * @returns {Promise<Workspace>}
 */
export async function open(source) {
  const resolved = await source;
  let instance;

  if (typeof Response !== "undefined" && resolved instanceof Response) {
    ({ instance } = await WebAssembly.instantiateStreaming(resolved, {}));
  } else {
    ({ instance } = await WebAssembly.instantiate(resolved, {}));
  }

  return new Workspace(instance.exports);
}

/** セルの状態。`Workspace.snapshot()` が返すもの。 */
export class Workspace {
  constructor(exports) {
    this.wasm = exports;
    this.handle = exports.mr_workspace_new();
    if (this.handle === 0) {
      throw new Error("ワークスペースを作れませんでした");
    }
  }

  /** wasm の線形メモリ。**伸びると差し替わるので、毎回取り直すこと。** */
  get memory() {
    return new Uint8Array(this.wasm.memory.buffer);
  }

  /**
   * 文字列を wasm 側へ渡して関数を呼ぶ。確保した領域は必ず返す。
   * @private
   */
  withString(text, call) {
    const bytes = encoder.encode(text);
    const ptr = this.wasm.mr_alloc(bytes.length);
    if (ptr === 0 && bytes.length > 0) {
      throw new Error("wasm 側で領域を確保できませんでした");
    }
    try {
      // メモリはここで取り直す。mr_alloc が線形メモリを伸ばした可能性がある。
      this.memory.set(bytes, ptr);
      return call(ptr, bytes.length);
    } finally {
      this.wasm.mr_free(ptr, bytes.length);
    }
  }

  /**
   * 直近の呼び出しが置いた文字列を取り出す。
   * @private
   */
  takeResult(len) {
    if (len === 0) return "";
    const ptr = this.wasm.mr_result_ptr();
    return decoder.decode(this.memory.subarray(ptr, ptr + len));
  }

  /**
   * セルを足す。返り値はセル番号。
   * @param {string} source
   * @returns {number}
   */
  addCell(source) {
    // mr_add_cell は u64 を返すので、JS 側には BigInt で来る。
    return Number(
      this.withString(source, (ptr, len) =>
        this.wasm.mr_add_cell(this.handle, ptr, len),
      ),
    );
  }

  /**
   * セルを書き換える。
   * @returns {boolean} 成功したか
   */
  updateCell(id, source) {
    const status = this.withString(source, (ptr, len) =>
      this.wasm.mr_update_cell(this.handle, BigInt(id), ptr, len),
    );
    return status === 0;
  }

  /**
   * セルを消す。
   * @returns {boolean} 成功したか
   */
  removeCell(id) {
    return this.wasm.mr_remove_cell(this.handle, BigInt(id)) === 0;
  }

  /**
   * 再計算が必要なセルを評価する。
   *
   * 返る `order` が「実際に再計算されたセル」である。**UI が見せるべきは
   * これ。** 変更したセル以外に何が動いたかが、この道具の主張そのもの。
   *
   * @returns {{evaluated: number, failed: number, order: number[]}}
   */
  evaluate() {
    const len = this.wasm.mr_evaluate(this.handle);
    const json = this.takeResult(len);
    return json ? JSON.parse(json) : { evaluated: 0, failed: 0, order: [] };
  }

  /**
   * 現在の状態をまるごと取る。
   * @returns {{revision: number, cells: Cell[], cycles: number[][]}}
   */
  snapshot() {
    const len = this.wasm.mr_snapshot(this.handle);
    const json = this.takeResult(len);
    return json ? JSON.parse(json) : { revision: 0, cells: [], cycles: [] };
  }

  /** ワークスペースを捨てる。 */
  close() {
    this.wasm.mr_workspace_free(this.handle);
    this.handle = 0;
  }
}

/**
 * セルの状態を、UI が出すべき一言に畳む。
 *
 * 優先順位が意味を持つ。**「循環」と「未解決」は値より先に伝えるべき情報**
 * であり、鮮度はその次である。
 *
 * @param {Cell} cell
 * @returns {{tone: string, label: string, detail: string}}
 */
export function statusOf(cell) {
  if (cell.inCycle) {
    return { tone: "cycle", label: "循環", detail: "依存が循環しています" };
  }
  if (cell.resolution === "Unresolved") {
    return {
      tone: "unresolved",
      label: "未解決",
      detail: `${cell.missing.join(", ")} がありません`,
    };
  }
  if (cell.error) {
    return { tone: "error", label: "エラー", detail: cell.error };
  }
  if (cell.resolution === "Ambiguous") {
    return {
      tone: "ambiguous",
      label: "曖昧",
      detail: `${cell.conflicts.join(", ")} の定義が複数あります`,
    };
  }
  switch (cell.freshness) {
    case "Clean":
      return { tone: "clean", label: "最新", detail: "" };
    case "Dirty":
      return { tone: "stale", label: "要再計算", detail: "上流が変わりました" };
    case "MaybeDirty":
      return {
        tone: "stale",
        label: "要確認",
        detail: "上流が変わった可能性があります",
      };
    default:
      return { tone: "pending", label: "未計算", detail: "" };
  }
}
