#!/bin/sh
# Corpus tests for the grammar, then a headless Neovim pass over every
# meshfox document in the repo. Run ./build.sh first.
set -eu
cd "$(dirname "$0")"
(cd grammar && tree-sitter test)
repo=$(git rev-parse --show-toplevel)
# shellcheck disable=SC2046
nvim --headless -u NONE -l test/check.lua \
  "$repo/README.md" "$repo/SPEC.md" "$repo/editors/neovim/README.md" \
  $(find "$repo" -name '*.canvas.md' -not -path '*/node_modules/*' -not -path '*/target/*' -not -path '*/.git/*')
