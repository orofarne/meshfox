#!/usr/bin/env bash
# Smoke test for a built meshfox image, run before it is pushed anywhere:
#
#   docker/smoke.sh <image> <slim|full> <version, e.g. 0.27.0>
#
# Starts the image the way users do (a mounted directory, the host's uid, a
# token) and checks what the image promises: the gateway answers and wants the
# token, the coordinator inside the container is what `docker exec … meshfox`
# talks to, blocks run as the mounted directory's owner, tables work, and the
# `full` tools are there.
set -euo pipefail

image=${1:?usage: smoke.sh <image> <slim|full> <version>}
variant=${2:?usage: smoke.sh <image> <slim|full> <version>}
version=${3:?usage: smoke.sh <image> <slim|full> <version>}

name="mfx-smoke-$$"
work=$(mktemp -d)
cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

step() { printf '== %s\n' "$*"; }
fail() { printf 'smoke test failed: %s\n' "$*" >&2; docker logs "$name" >&2 || true; exit 1; }
in_container() { docker exec "$name" "$@"; }
# Run a command in the container and require its output to contain a pattern.
# The output is captured first: `cmd | grep -q` would kill the command with
# SIGPIPE as soon as grep matches, which `pipefail` reports as a failure.
expect_output() {
  local pattern=$1 out
  shift
  out=$(in_container "$@" 2>&1) || fail "'$*' failed: $out"
  grep -q -- "$pattern" <<<"$out" || fail "'$*' did not print '$pattern': $out"
}

cat > "$work/smoke.canvas.md" <<'CANVAS'
<!-- meshfox:canvas -->
# Smoke
<!-- meshfox:node id="root" -->

```bash name="greet"
echo "hello from $(id -u)"
echo done > /work/made-by-block.txt
```
CANVAS
printf 'a,b\n1,x\n2,y\n' > "$work/data.csv"
cat > "$work/table.canvas.md" <<'CANVAS'
<!-- meshfox:canvas -->
# Table
<!-- meshfox:node id="root" -->

## Data
<!-- meshfox:node id="tbl" type="file" parent="root" display="table" -->

[data](./data.csv)
CANVAS
chmod -R a+rwX "$work"

step "start $image as uid $(id -u)"
docker run -d --name "$name" --user "$(id -u):$(id -g)" -v "$work:/work" \
  -e MESHFOX_TOKEN=smoke "$image" >/dev/null

step "healthy"
status=starting
for _ in $(seq 60); do
  status=$(docker inspect -f '{{.State.Health.Status}}' "$name")
  [[ "$status" = healthy ]] && break
  sleep 1
done
[[ "$status" = healthy ]] || fail "container is $status"

step "version is v$version"
expect_output "v$version" meshfox --version

step "the gateway wants the token and accepts it"
code=$(in_container curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:4590/_/index.json)
[[ "$code" = 401 ]] || fail "anonymous index answered $code, expected 401"
expect_output smoke.canvas.md curl -fsS -H 'Authorization: Bearer smoke' http://127.0.0.1:4590/_/index.json

step "a block runs through the container's coordinator, as the mounted directory's owner"
expect_output "hello from $(id -u)" meshfox /work/smoke.canvas.md run greet
[[ -f "$work/made-by-block.txt" ]] || fail "the block's file did not reach the mounted directory"
expect_output smoke.canvas.md meshfox cores ls

step "check-updates says to pull a new image"
expect_output 'container image' meshfox check-updates

step "tables (DuckDB)"
index=$(in_container curl -fsS -H 'Authorization: Bearer smoke' http://127.0.0.1:4590/_/index.json)
slug=$(python3 -c "import sys,json; print([c['slug'] for c in json.load(sys.stdin)['canvases'] if c['path']=='table.canvas.md'][0])" <<<"$index")
expect_output '"name":"a"' curl -fsS -H 'Authorization: Bearer smoke' "http://127.0.0.1:4590/c/$slug/api/nodes/tbl/table"

if [[ "$variant" = full ]]; then
  step "full: python, node, chromium, pdf"
  in_container python3 -c 'import venv, sys; print(sys.version)' >/dev/null || fail "python3"
  in_container node --version >/dev/null || fail "node"
  in_container chromium --version >/dev/null || fail "chromium"
  in_container meshfox /work/smoke.canvas.md pdf -o /work/smoke.pdf >/dev/null || fail "meshfox pdf"
  [[ -s "$work/smoke.pdf" ]] || fail "no PDF was written"
fi

step "ok"
