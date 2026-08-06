// 実ブラウザで UI を操作して確かめる。
//
// `view.rs` のテストは「何を見せるか」の判断を固定するが、**DOM に実際に
// 届いているか**は見ていない。ここが最後の穴で、実際にバグを 2 件出した。
//
//   - `<For>` がキーの同じ行を作り直さないので、編集しても画面が変わらない
//   - MathLive の読み込み順で、初期セルだけ素の入力欄になる
//
// どちらも Rust のテストでは絶対に捕まらない。
//
// 使い方（Playwright は入れない。キャッシュ済みの chromium を CDP で叩く）:
//
//   scripts/build-web.sh --release
//   (cd web && python3 -m http.server 8099 &)
//   chrome --headless=new --remote-debugging-port=9222 http://127.0.0.1:8099/ &
//   node crates/mathrel-ui/tests/browser.mjs
//
// 環境変数 MATHREL_CDP_PORT / MATHREL_URL で差し替えられる。
const PORT = Number(process.env.MATHREL_CDP_PORT ?? 9222);
const PAGE = process.env.MATHREL_URL ?? "http://127.0.0.1:8099/";

const list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
const target = list.find((t) => t.type === "page" && t.url.startsWith(PAGE));
if (!target) {
  console.error("ページが見つかりません", list.map((t) => t.url));
  process.exit(1);
}

const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve) => (ws.onopen = resolve));

let seq = 0;
const pending = new Map();
ws.onmessage = (event) => {
  const msg = JSON.parse(event.data);
  if (msg.id && pending.has(msg.id)) {
    pending.get(msg.id)(msg);
    pending.delete(msg.id);
  }
};

function send(method, params = {}) {
  const id = ++seq;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((resolve) => pending.set(id, resolve));
}

async function evaluate(expression) {
  const reply = await send("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (reply.result?.exceptionDetails) {
    throw new Error(JSON.stringify(reply.result.exceptionDetails));
  }
  return reply.result?.result?.value;
}

const wait = (ms) => new Promise((r) => setTimeout(r, ms));

let failed = 0;
function check(name, condition, detail = "") {
  if (condition) {
    console.log(`  ✓ ${name}`);
  } else {
    failed += 1;
    console.log(`  ✗ ${name} ${detail}`);
  }
}

// セルの見え方を読み出す。
const READ = `
  (() => Array.from(document.querySelectorAll('.cell')).map((cell) => ({
    id: cell.querySelector('.id')?.textContent ?? '',
    source: cell.querySelector('.input')?.value ?? cell.querySelector('.input')?.textContent ?? '',
    value: cell.querySelector('.value')?.textContent ?? null,
    badge: cell.querySelector('.badge')?.textContent ?? '',
    recomputed: cell.classList.contains('recomputed'),
    tone: cell.className,
  })))()
`;

console.log("ブラウザでの操作");

// wasm の読み込みは一瞬では終わらない。セルが出るまで待つ。
await send("Page.enable");
await send("Page.reload", { ignoreCache: true });
let cells = [];
for (let attempt = 0; attempt < 60; attempt += 1) {
  await wait(250);
  try {
    cells = await evaluate(READ);
  } catch {
    cells = [];
  }
  if (cells.length > 0) break;
}
check("初期状態で 3 セル出ている", cells.length === 3, JSON.stringify(cells));
check("y = f(x) が 5 と出る", cells[2]?.value === "= 5", cells[2]?.value);

const fields = await evaluate(`
  (() => ({
    mathlive: !!customElements.get('math-field'),
    tags: Array.from(document.querySelectorAll('.cell .input')).map((el) => el.tagName.toLowerCase()),
  }))()
`);
check("MathLive が読み込まれている", fields.mathlive === true);
check(
  "数式セルはすべて MathLive で描かれる",
  fields.mathlive ? fields.tags.every((tag) => tag === "math-field") : true,
  JSON.stringify(fields.tags),
);

// [1] を書き換えて change を発火させる（実際の編集と同じ経路）。
await evaluate(`
  (() => {
    const input = document.querySelectorAll('.cell')[0].querySelector('.input');
    input.value = 'x = 3';
    input.dispatchEvent(new Event('change', { bubbles: true }));
  })()
`);
await wait(400);

cells = await evaluate(READ);
check("x が 3 になる", cells[0]?.value === "= 3", cells[0]?.value);
check("y が 10 になる", cells[2]?.value === "= 10", cells[2]?.value);
check("x は再計算された印がつく", cells[0]?.recomputed === true);
check(
  "f は再計算されない（企画書 P1 の主張）",
  cells[1]?.recomputed === false,
  JSON.stringify(cells[1]),
);
check("y は再計算された印がつく", cells[2]?.recomputed === true);

// 未解決の参照。
await evaluate(`
  (() => {
    document.querySelector('.draft').value = 'w = nowhere + 1';
    document.querySelector('.draft').dispatchEvent(new Event('input', { bubbles: true }));
    document.querySelector('.add').click();
  })()
`);
await wait(400);
cells = await evaluate(READ);
check("未解決のセルが未解決と出る", cells[3]?.badge === "未解決", cells[3]?.badge);

// 仮置きを足すと警告が出る。
await evaluate(`
  (() => {
    document.querySelectorAll('.mode')[2].click();
    document.querySelector('.draft').value = 'hard : 難しい命題';
    document.querySelector('.draft').dispatchEvent(new Event('input', { bubbles: true }));
    document.querySelector('.add').click();
  })()
`);
await wait(400);

const warning = await evaluate(`
  (() => {
    const box = document.querySelector('.warning');
    if (!box) return null;
    return {
      headline: box.querySelector('.headline')?.textContent ?? '',
      lines: Array.from(box.querySelectorAll('li')).map((li) => li.textContent),
      trustline: document.querySelector('.trustline')?.textContent ?? '',
    };
  })()
`);
check("満点でないと警告が出る", warning !== null);
check(
  "全体の信頼度が下がる",
  warning?.trustline?.includes("Assumed"),
  warning?.trustline,
);
check(
  "仮置きが理由つきで挙がる",
  warning?.lines?.some((line) => line.includes("誰も検査していません")),
  JSON.stringify(warning?.lines),
);

// 引用すると下流も落ちる。
await evaluate(`
  (() => {
    document.querySelectorAll('.mode')[1].click();
    document.querySelector('.draft').value = 'main : a = a cites hard';
    document.querySelector('.draft').dispatchEvent(new Event('input', { bubbles: true }));
    document.querySelector('.add').click();
  })()
`);
await wait(400);
const dragged = await evaluate(`
  (() => Array.from(document.querySelectorAll('.warning li')).map((li) => li.textContent))()
`);
check(
  "引用した定理も原因つきで挙がる",
  dragged.some((line) => line.includes("原因は")),
  JSON.stringify(dragged),
);

const shot = await send("Page.captureScreenshot", { format: "png" });
if (shot.result?.data) {
  const { writeFile } = await import("node:fs/promises");
  await writeFile(
    "/home/vyuma/.claude/jobs/114abfd7/tmp/ui-after.png",
    Buffer.from(shot.result.data, "base64"),
  );
}

console.log(failed === 0 ? "\nすべて通過" : `\n${failed} 件失敗`);
ws.close();
process.exit(failed === 0 ? 0 : 1);
