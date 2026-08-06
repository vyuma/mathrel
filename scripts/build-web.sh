#!/usr/bin/env bash
# UI をビルドして web/ に出す。
#
# UI クレートはワークスペースから外してある（ルート Cargo.toml の exclude）。
# Leptos が rustc 1.88 を要求する一方、開発機は環境変数 RUSTUP_TOOLCHAIN で
# 1.87 に固定されているためである。ここで 1.88 を明示する。環境変数を書き
# 換えると他のプロジェクトにも影響するので、触らない。
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/crates/mathrel-ui"

export RUSTUP_TOOLCHAIN="${MATHREL_UI_TOOLCHAIN:-1.88.0}"

if [ "${1:-}" = "serve" ]; then
    exec trunk serve --address 0.0.0.0
fi

trunk build "$@"
echo
echo "出力: web/"
ls -la "$root/web"
