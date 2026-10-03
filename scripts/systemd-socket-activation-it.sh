#!/usr/bin/env bash
# End-to-end check of linux/systemd/meshfox.{socket,service} under a real
# `systemd --user`: socket activation, on-demand start, a clean stop that
# leaves the socket (and takes the workers with it), and re-activation.
# It also checks that a worker the service spawns can read a secret from the
# keyring (`secret_store = "keychain"`). Linux with a systemd user session
# only (a lima VM, say), plus `gnome-keyring`, `secret-tool` and `python3`.
# Installs the units for the current user and removes them again on exit.
#
#   scripts/systemd-socket-activation-it.sh [PATH_TO_MESHFOX]
set -euo pipefail
cd "$(dirname "$0")/.."

bin="${1:-${CARGO_TARGET_DIR:-target}/debug/meshfox}"
[[ -x "$bin" ]] || { echo "no meshfox binary at $bin (cargo build -p meshfox-cli)" >&2; exit 1; }

sock="${XDG_RUNTIME_DIR:?needs a systemd user session}/meshfox/daemon.sock"
work="$(mktemp -d)"
export MESHFOX_SERVER_SOCKET="$sock"

cleanup() {
  mf secret rm API_TOKEN --canvas "$work/proj/doc.canvas.md" >/dev/null 2>&1 || true
  pkill -u "$(id -u)" -x gnome-keyring-d 2>/dev/null || true
  linux/install-user-units.sh --uninstall >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok:   $*"; }
mf() { "$HOME/.local/bin/meshfox" "$@"; }
active() { systemctl --user is-active "$1" 2>/dev/null || true; }

get_port() {
  python3 - "$sock" "$1" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(30)
s.connect(sys.argv[1])
s.sendall((json.dumps({"op": "get_port", "canvas_path": sys.argv[2]}) + "\n").encode())
reply = json.loads(s.makefile().readline())
print(reply["port"])
PY
}

printf '<!-- meshfox:canvas -->\n# Doc\n' > "$work/doc.canvas.md"
canvas="$(realpath "$work/doc.canvas.md")"

linux/install-user-units.sh "$bin" >/dev/null

# 1. The socket is there, the service is not running yet.
[[ "$(active meshfox.socket)" == active ]] || fail "meshfox.socket is not active"
[[ -S "$sock" ]] || fail "no socket at $sock"
[[ "$(active meshfox.service)" != active ]] || fail "service should not run before the first connection"
[[ "$(stat -c %a "$sock")" == 600 ]] || fail "socket mode is $(stat -c %a "$sock"), want 600"
[[ "$(stat -c %a "$(dirname "$sock")")" == 700 ]] || fail "socket dir mode is not 700"
ok "socket active and private; service idle"

# 2. The first connection starts the service.
[[ "$(mf cores ls)" == "no cores running" ]] || fail "cores ls: $(mf cores ls)"
[[ "$(active meshfox.service)" == active ]] || fail "service did not start on connect"
pid1="$(systemctl --user show -p MainPID --value meshfox.service)"
ok "first connection started the service (pid $pid1)"

# 3. It spawns, lists and kills cores.
port="$(get_port "$canvas")"
[[ -n "$port" && "$port" != 0 ]] || fail "get_port returned '$port'"
mf cores ls | grep -q "$canvas.*$port" || fail "core not listed: $(mf cores ls)"
ok "core spawned on port $port and listed"

# 4. Stopping the service takes the workers with it but leaves the socket.
systemctl --user stop meshfox.service
for _ in $(seq 50); do pgrep -f "meshfox view $canvas" >/dev/null || break; sleep 0.1; done
pgrep -f "meshfox view $canvas" >/dev/null && fail "worker survived the service stop"
[[ "$(active meshfox.socket)" == active && -S "$sock" ]] || fail "socket vanished with the service"
ok "stop: workers gone, socket kept"

# 5. The next connection starts a fresh service.
[[ "$(mf cores ls)" == "no cores running" ]] || fail "after restart: $(mf cores ls)"
pid2="$(systemctl --user show -p MainPID --value meshfox.service)"
[[ "$pid2" != 0 && "$pid2" != "$pid1" ]] || fail "expected a new service process ($pid1 -> $pid2)"
ok "re-activated on demand (pid $pid2)"

# 6. A clean exit (SIGTERM from outside) behaves the same: no restart loop,
#    socket stays, next connection works.
kill -TERM "$pid2"
for _ in $(seq 50); do [[ "$(active meshfox.service)" != active ]] && break; sleep 0.1; done
[[ "$(active meshfox.service)" != active ]] || fail "service ignored SIGTERM"
[[ "$(active meshfox.socket)" == active && -S "$sock" ]] || fail "socket vanished after SIGTERM"
sleep 1
[[ "$(active meshfox.service)" != active ]] || fail "service was restarted by itself"
[[ "$(mf cores ls)" == "no cores running" ]] || fail "after SIGTERM: $(mf cores ls)"
ok "SIGTERM: clean exit, socket kept, no self-restart, re-activates"

# 7. Secrets: a worker spawned by the service reads the keyring. The service
#    runs with the user manager's environment, not this shell's, so this is
#    what proves the session bus is reachable from there and a stored secret
#    comes through.
echo -n throwaway-password | gnome-keyring-daemon --unlock --components=secrets >/dev/null
mkdir -p "$work/proj/.meshfox"
printf 'secret_store = "keychain"\n' > "$work/proj/.meshfox/config.toml"
cat > "$work/proj/doc.canvas.md" <<'CANVAS'
<!-- meshfox:canvas -->
# Doc
<!-- meshfox:var name="API_TOKEN" secret -->
<!-- meshfox:node id="doc" -->

```bash name="show" env="API_TOKEN"
echo "token=$API_TOKEN"
```
CANVAS
proj_canvas="$(realpath "$work/proj/doc.canvas.md")"
printf 's3cret-from-keyring' | mf secret set API_TOKEN --canvas "$proj_canvas" >/dev/null
result="$(mf "$proj_canvas" run show)"
[[ "$result" == *"token=s3cret-from-keyring"* ]] || fail "run through the service did not see the secret: $result"
svc_pid="$(systemctl --user show -p MainPID --value meshfox.service)"
worker_pid="$(pgrep -f "meshfox view $proj_canvas" | head -1)"
[[ -n "$worker_pid" ]] || fail "no worker for $proj_canvas"
[[ "$(ps -o ppid= -p "$worker_pid" | tr -d ' ')" == "$svc_pid" ]] \
  || fail "the worker is not a child of the service (so this did not go through it)"
ok "a worker spawned by the service read the secret from the keyring"

echo "all good"
