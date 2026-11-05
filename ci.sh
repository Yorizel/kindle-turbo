#!/usr/bin/env bash
# CI local: formatação, lint estrito e testes (pelo resource-run do host).
# Uso: ./ci.sh            (rápido)
#      ./ci.sh --full     (inclui testes com livros reais e rede: --ignored)
set -euo pipefail
cd "$(dirname "$0")"
RR=(/home/yorizel/.local/bin/resource-run --wait)
step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

step "fmt"
cargo fmt --all --check

step "clippy (pedantic + nursery + cargo + restriction, -D warnings)"
"${RR[@]}" cargo clippy --all-targets --release -- -D warnings

step "testes"
"${RR[@]}" cargo test --release

if [[ "${1:-}" == "--full" ]]; then
  step "testes reais (livros + AniList)"
  "${RR[@]}" cargo test --release -- --ignored
fi
printf '\n\033[32mCI local OK\033[0m\n'
