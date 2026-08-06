#!/usr/bin/env bash
#
# codex.sh — このリポジトリで Codex CLI を非対話実行する。
#
# 素で `codex exec` を叩くと、この環境では 3 つの落とし穴がある。
#
#   1. git 管理外のディレクトリだと "Not inside a trusted directory" で即死する。
#      このリポジトリは git 化したので今は起きないが、複製して使う場合に
#      備えて `--skip-git-repo-check` を付けてある。
#
#   2. 標準入力が開いたままだと、Codex は「追加の入力」を待ち続ける。
#      端末なしで呼ぶと止まって見える。`< /dev/null` で閉じる。
#
#   3. ~/.codex/config.toml が `model_reasoning_effort = "xhigh"` になっている。
#      複数ファイルを読ませるレビューだと 10 分では終わらない。用途に応じて
#      下げる（既定は medium）。
#
# さらに、出力を `| tail` に通すと最後まで何も見えない。進行を見たいときは
# パイプせずファイルへ落とすこと。このスクリプトは常にファイルへ落とす。
#
# 使い方:
#   scripts/codex.sh "プロンプト"
#   scripts/codex.sh -e xhigh "じっくり考えさせたいプロンプト"
#   scripts/codex.sh -w "ファイルを書き換えてよい依頼"
#   echo "長いプロンプト" | scripts/codex.sh -
#
# 出力は最終メッセージが --out のファイルに、全ログが同名 .log に残る。

set -euo pipefail

effort="medium"
sandbox="read-only"
out=""

usage() {
    sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        -e|--effort)   effort="$2"; shift 2 ;;
        -w|--write)    sandbox="workspace-write"; shift ;;
        -o|--out)      out="$2"; shift 2 ;;
        -h|--help)     usage 0 ;;
        --)            shift; break ;;
        # 裸の `-` は「標準入力から読む」の意味。`-*` より先に置くこと
        # （後ろに置くと選択肢と誤認され、README の使い方が動かない）。
        -)             break ;;
        -*)            echo "知らない選択肢: $1" >&2; usage 1 ;;
        *)             break ;;
    esac
done

if [ $# -eq 0 ]; then
    echo "プロンプトがありません" >&2
    usage 1
fi

prompt="$1"
if [ "$prompt" = "-" ]; then
    prompt="$(cat)"
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
if [ -z "$out" ]; then
    mkdir -p "$root/.codex-out"
    out="$root/.codex-out/$(date +%Y%m%d-%H%M%S).md"
fi
log="${out%.md}.log"

echo "codex: effort=$effort sandbox=$sandbox" >&2
echo "  最終メッセージ: $out" >&2
echo "  全ログ:         $log" >&2

# `< /dev/null` が要る（上記 2）。`| tail` はバッファするので使わない。
codex exec \
    --skip-git-repo-check \
    --sandbox "$sandbox" \
    --color never \
    -C "$root" \
    -c model_reasoning_effort="$effort" \
    -o "$out" \
    "$prompt" \
    < /dev/null \
    > "$log" 2>&1

status=$?
if [ $status -ne 0 ]; then
    echo "codex が異常終了しました（exit=$status）。ログ: $log" >&2
    tail -20 "$log" >&2
    exit $status
fi

cat "$out"
