# mathrel

Relational Math Kernel — 数式・定義・宣言を、相互に依存する数学オブジェクトとして管理する実験的な Rust ワークスペース。

新しい CAS を作るものではない。数式を文字列ではなく `requires` / `provides` を持つオブジェクトとして登録し、そこから依存グラフを導出して、**変更が波及する範囲を機械的に特定する**ことだけを担う。

企画と背景は [`docs/企画書第3版.md`](docs/企画書第3版.md)。証明支援系と AI を噛ませる設計は [`docs/検証層の設計.md`](docs/検証層の設計.md)。

---

## 何を解くのか

数学の作業では、定義を 1 つ変えると、それに依存する計算がすべて古くなる。**現状、この「どこが古くなったか」を追跡しているのは人間の頭だけである。** 紙のノートにも、LaTeX にも、Jupyter にも、この情報は保存されない。

```
$ mathrel --demo
-- 企画書 P1 の完了条件 --
> x = 2
[1] x = 2  → 2  (Clean)
> f(t) = t^2 + 1
[2] f(t) = t^2 + 1  (Clean)
> y = f(x)
[3] y = f(x)  → 5  (Clean)
> :set 1 x = 3
[1] x = 3  → 3  (Clean)
  再計算: [3]
  ↑ x だけを変えた。再計算されたのは y だけで、f は触っていない。
```

---

## クレート構成

```
mathrel-kernel  ←  mathrel-verify  ←  mathrel-host  ←  mathrel-cli
                                                    ←  mathrel-wasm
```

| クレート | 役割 |
|---|---|
| `mathrel-kernel` | 依存追跡と鮮度管理のみ。**評価しない。パースしない。I/O しない。** |
| `mathrel-verify` | 証明義務・証明支援系バックエンド・信頼度の伝播 |
| `mathrel-host` | パース、依存の抽出、評価、セル管理 |
| `mathrel-cli` | 対話シェル `mathrel` |
| `mathrel-ui` | 画面（Leptos → wasm）。ワークスペース外 |
| `mathrel-wasm` | 素の C ABI（JS から叩きたい場合の入口） |

### カーネルは 2 つのことしか知らない

1. 誰が誰に依存しているか
2. いま何が古いか

計算はホストが行い、結果の**指紋**だけをカーネルへ報告する。この線引きにより、カーネルは純粋・同期・I/O なしになり、素朴実装との等価性を性質テストで確認できる。

**依存の登録は明示的でなければならない。** カーネルが式の中身をパースして依存を推測することは禁止している。解析で依存を推測する系は、解析の限界がそのまま健全性の穴になるからである。

---

## 証明支援系と AI

`y = f(x)` が古くなるのと、`定理: f は単調である` が古くなるのは同じ現象である。したがって**証明はカーネルにとって評価と同型**であり、新しい追跡機構は要らない。

```rust
use mathrel_verify::{Obligation, ProofSpace, Trust, TrivialVerifier};

let mut space = ProofSpace::new();
space.add_definition("f", "def f (t : Nat) : Nat := t");

// 補題は「これは既知」と宣言しただけ。誰も検査していない。
let lemma = space.add_assumption(Obligation::new("f_id", "f t = t").using(&["f"]));
// 定理そのものは検証器が完全に検査した。
let theorem = space.add_obligation(Obligation::new("main", "f t = f t").citing(&["f_id"]));
space.verify_all(&TrivialVerifier);

assert_eq!(space.own_trust(theorem), Trust::Checked);       // 自分は検査済み
assert_eq!(space.effective_trust(theorem), Trust::Assumed); // だが仮置きに依存している
```

**早期カットオフは証明でこそ本質的である。** 算術の再計算はマイクロ秒だが、Lean の再検査は秒〜分かかる。定義を触るたびに全証明を検査し直す設計では実用にならない。

数式セルと検証義務は**同じワークスペースの同じグラフ**に載る。`x = 2` というセルと、`x` に言及する定理が繋がる。

```
> x = 2
> :assume hard : 難しい命題
⚠ 信頼度が満点でないものが 1 件あります: [2]（:trust で内訳）
> :thm main : a = a cites hard
[3] main : a = a  (Checked → 実効 Assumed)
⚠ 信頼度が満点でないものが 2 件あります: [2] [3]（:trust で内訳）
```

**満点でない信頼度は黙って通さない。** どの命令の後でも警告が出て、`:trust` が原因のセルを名指しする。

**AI は提案し、検証器が判定する。** `ProofSynthesizer` は文字列しか返せず、`Verdict` を返す手段を型として持たない。AI が出した証明を Lean が拒否したら、それは単に失敗した試行である。

Lean バックエンドは一時ファイルへ書き出して `lake env lean <path>` を呼ぶ。`sorry` の検出は警告文の一致ではなく、`#print axioms` が報告する `sorryAx` で行う（壊れたときに信頼度が上がる側へ倒れないため）。設計の詳細と、外部レビューで見つかった誤りは [`docs/検証層の設計.md`](docs/検証層の設計.md) §6.0。

---

## 使う

```sh
cargo test --workspace          # 232 テスト
cargo run -p mathrel-cli -- --demo
printf 'x = 2\ny = x + 1\n:list\n' | cargo run -q -p mathrel-cli
```

対話シェルの命令は `:help` で出る。

### 画面

```sh
scripts/build-web.sh --release     # web/ に出る
scripts/build-web.sh serve         # http://localhost:8080
```

画面は Leptos（Rust → wasm）で書いてある。JS のフレームワークではないので、
**Rust と画面のあいだに JSON の境界がない**。`Workspace` / `Trust` /
`WeakLink` が本物の型のまま届き、フィールド名を変えれば画面のほうが
コンパイルエラーになる。

数式入力は MathLive（企画書 §12）。読み込めない環境では素のテキスト入力に
落ちる。

UI クレートは**意図的にワークスペースから外してある**。Leptos が rustc 1.88 を
要求する一方、開発機は環境変数 `RUSTUP_TOOLCHAIN` で 1.87 に固定されている
ためで、members に入れると `cargo test --workspace` まで巻き添えになる。

```sh
# 生の C ABI（JS から叩きたい場合。画面は使っていない）
cargo build -p mathrel-wasm --target wasm32-unknown-unknown --release
```

---

## テスト

| 場所 | 内容 |
|---|---|
| `crates/mathrel-kernel/tests/scenarios.rs` | T01〜T15 のシナリオテスト |
| `crates/mathrel-kernel/tests/properties.rs` | P1〜P6 の性質テスト（各 1000 ケース）+ 深さ 5 の連鎖 |
| `crates/mathrel-kernel/tests/support/` | 素朴な参照実装（オラクル） |
| `crates/mathrel-verify/tests/proof_space.rs` | 証明の依存追跡と信頼度の伝播 |
| `crates/mathrel-host/tests/proofs.rs` | 数式セルと証明が 1 つのグラフに載ること |
| `crates/mathrel-ui/src/view.rs` | 画面が何を見せるかの判断（ブラウザ不要） |
| `crates/mathrel-ui/tests/browser.mjs` | 実ブラウザでの操作（CDP 直叩き） |

性質テストは素朴実装との等価性を確認する。生成器が空振りしていないこと（循環・未解決・曖昧・間接下流を実際に踏んでいること）は `generators_cover_interesting_states` が見張る。

Lean は入っていなくてもテストは通る。外部プロセスの起動は `CommandRunner` trait で切ってあり、**道具の有無でテストの通り方が変わらない**ようにしてある。

画面のテストは 2 段。判断（何を見せるか）は `view.rs` でブラウザなしに固定し、
DOM へ実際に届いているかは `tests/browser.mjs` が実ブラウザで確かめる。
後者は実際にバグを 2 件出した（`<For>` がキーの同じ行を作り直さないので編集が
反映されない／MathLive の読み込み順で初期セルだけ素の入力欄になる）。
どちらも Rust のテストでは捕まらない類である。

---

## 開発の道具

### Codex

```sh
scripts/codex.sh "プロンプト"                  # 読み取り専用、reasoning=medium
scripts/codex.sh -e xhigh "じっくり考えさせる"   # 時間はかかる
scripts/codex.sh -w "書き換えてよい依頼"        # workspace-write
echo "長いプロンプト" | scripts/codex.sh -
```

素で `codex exec` を叩くとこの環境では止まって見える。理由は 3 つあり、スクリプトはすべて回避している。

| 症状 | 原因 | 対処 |
|---|---|---|
| `Not inside a trusted directory` で即死 | git 管理外だと弾かれる（現在は git 化済みなので起きない） | `--skip-git-repo-check`（保険として付けてある） |
| 何も出力せず止まったまま | 標準入力が開いていて「追加の入力」を待ち続ける | `< /dev/null` |
| 10 分経っても終わらない | `~/.codex/config.toml` が `model_reasoning_effort = "xhigh"` | `-c model_reasoning_effort=medium` |

加えて、`codex exec ... | tail` は**最後まで 1 行も表示されない**（`tail` が全部バッファする）。進行を見たいならパイプせずファイルへ落とすこと。スクリプトは `.codex-out/` に最終メッセージと全ログを残す。

なお `-m` でのモデル指定は ChatGPT アカウントでは弾かれるモデルがある（`gpt-5.1-codex` は不可）。既定モデルは `~/.codex/config.toml` の `model` に従う。

### MCP

`codex mcp add <名前> -- <コマンド>` で Codex に MCP サーバを足せる。Lean の MCP サーバを繋げば証明の提案の質は上がるが、**信頼度は上がらない**。Codex が自分で確かめたと言っても自己申告であり、`Trust::Checked` になるのはこちらの `Verifier` が通したときだけである（`docs/検証層の設計.md` ADR-007）。

---

## 現在地

企画書 §8 の段階計画に対して:

- **P0（カーネル v0.1）**: 完了。シナリオテストと性質テストが green
- **P1（評価ループ + CLI）**: 完了。上記の完了条件を CLI で確認できる
- **P1.5（検証層の骨格）**: 完了。数式セルと証明が 1 つのグラフに載る。Lean バックエンドは実地未投入（[#23](https://github.com/vyuma/mathrel/issues/23)）
- **P2（UI 最小）**: 画面ができた（セル入力・状態表示・依存の一覧・信頼度の警告）。**ゲートである H3 の測定は未実施**（[#8](https://github.com/vyuma/mathrel/issues/8)）

未解決の設計課題は [`docs/検証層の設計.md`](docs/検証層の設計.md) §6.1〜6.2 に記録してある。

---

## ライセンス

MIT ライセンス（[LICENSE-MIT](LICENSE-MIT)）または Apache License 2.0
（[LICENSE-APACHE](LICENSE-APACHE)）のいずれかを選べます。

このリポジトリへ意図的に提出された貢献は、Apache-2.0 の定義に従い、追加の
条件なしに上記のデュアルライセンスで提供されたものとみなします。
