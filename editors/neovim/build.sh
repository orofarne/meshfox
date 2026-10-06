#!/bin/sh
# Build the meshfox tree-sitter parser into ./parser/meshfox.so, the place
# Neovim looks for it once this directory is on 'runtimepath'.
#
#   ./build.sh                  # meshfox only
#   ./build.sh --with-starlark  # also tree-sitter-starlark, for ```starlark constraint fences
#
# Needs the `tree-sitter` CLI and a C compiler. src/parser.c is committed, so
# `tree-sitter generate` (and node) is not needed just to build.
set -eu
cd "$(dirname "$0")"

# Pin so a rebuild is reproducible.
STARLARK_REPO=https://github.com/tree-sitter-grammars/tree-sitter-starlark
STARLARK_REF=v1.3.0

case "$(uname -s)" in
  Darwin) ext=so ;;  # Neovim loads parser/<lang>.so on every platform
  *) ext=so ;;
esac

mkdir -p parser
tree-sitter build -o "parser/meshfox.$ext" grammar
echo "built parser/meshfox.$ext"

if [ "${1:-}" = "--with-starlark" ]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  git clone --depth 1 --branch "$STARLARK_REF" "$STARLARK_REPO" "$tmp/starlark"
  tree-sitter build -o "parser/starlark.$ext" "$tmp/starlark"
  mkdir -p queries/starlark
  cp "$tmp/starlark/queries/highlights.scm" queries/starlark/highlights.scm
  echo "built parser/starlark.$ext"
fi
