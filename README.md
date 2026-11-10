<!-- meshfox:canvas -->
# meshfox
<!-- meshfox:node id="meshfox" -->

```
 /\_/\
( ¬‿¬ )──●──●──●
  c c
```

[Website](https://meshfox.orofarne.net/) · [Online editor](https://meshfox-editor.orofarne.net/) · [GitHub](https://github.com/orofarne/meshfox) · [Download binary](https://github.com/orofarne/meshfox/releases)

[![Quality Gate Status](https://sonarcloud.io/api/project_badges/measure?project=orofarne_meshfox&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=orofarne_meshfox)

Install on Linux/macOS in one line:

```sh
curl -fsSL https://raw.githubusercontent.com/orofarne/meshfox/main/scripts/install.sh | sh
```

Or install the Meshfox CLI on macOS or Linux with Homebrew (Apple Silicon/ARM64
and Intel/x86-64):

```sh
brew install orofarne/tap/meshfox
```

Update a Homebrew installation with:

```sh
brew update
brew upgrade orofarne/tap/meshfox
```

Use Homebrew to update this installation rather than `meshfox check-updates`.
The formula installs the CLI, including its terminal and web interfaces.

Or run it in Docker (`linux/amd64` and `linux/arm64`, nothing to install; see
"Docker image" below for the options). There are two images. **`slim`** (the
default) is meshfox with `bash`, `git` and DuckDB (for tables):

```sh
docker run --user "$(id -u):$(id -g)" -p 127.0.0.1:4590:4590 \
  -e MESHFOX_TOKEN=change-me -v "$PWD":/work ghcr.io/orofarne/meshfox
```

**`full`** adds Python 3, Node.js and Chromium (for blocks in those languages
and for `meshfox pdf`):

```sh
docker run --user "$(id -u):$(id -g)" -p 127.0.0.1:4590:4590 \
  -e MESHFOX_TOKEN=change-me -v "$PWD":/work ghcr.io/orofarne/meshfox:full
```

Or on Windows, through WSL2 (no native build; see "Windows (WSL2)" below). This
PowerShell snippet downloads [`scripts/meshfox-wsl.ps1`](./scripts/meshfox-wsl.ps1)
into the current directory and runs it: it installs the WSL distribution if needed, installs or updates
meshfox and the DuckDB CLI inside it, and serves the canvases in `-CanvasDir`
(a Windows folder) at `http://localhost:4590/`:

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/orofarne/meshfox/main/scripts/meshfox-wsl.ps1 -OutFile meshfox-wsl.ps1
powershell -ExecutionPolicy Bypass -File .\meshfox-wsl.ps1 -CanvasDir "$env:USERPROFILE\meshfox"
```

Package maintainers can disable self-updating without rebuilding the binary:
install a marker at `<prefix>/share/meshfox/install-method` alongside
`<prefix>/bin/meshfox`. Its contents may be `homebrew`, `deb`, `pacman` or `docker` for
an update hint. `meshfox check-updates` (including `--yes`) then reports that
the installation is managed by a package manager and exits without contacting
GitHub or replacing the executable. Empty, unknown, or unreadable markers also
disable self-updating. The prefix comes from the executable's actual path after
resolving symlinks; no global marker directories are searched. A standalone
installation without a marker keeps its normal self-update behavior. Include
the marker in the package so it is installed and removed together with the
binary; do not include it in the standalone release archive.

[VS Code extension](https://marketplace.visualstudio.com/items?itemName=Orofarne.meshfox-vscode)

Or build and install from source yourself — this block calls "Install" in [DEVELOPMENT.canvas.md](./DEVELOPMENT.canvas.md) (a native `meshfox` fence, see SPEC.md's "Calling another canvas"), runnable from anywhere via `meshfox README.md run install-from-source` too.

```meshfox name="install-from-source"
canvas: DEVELOPMENT.canvas.md
run: install
```

An interactive canvas: a hierarchical, node-based document where nodes hold Markdown, and code blocks inside that Markdown can be run.

Meshfox pulls together several documentation patterns that are usually spread across separate tools into one file format: the mindmap-style canvas of Miro and Obsidian, Livebook’s lightweight markdown-compatible storage, code-in-document execution from Jupyter Notebook and Livebook, and Make’s dependency graph for running scripts. A built-in Starlark constraint system (meshfox check) lets a canvas enforce its own consistency rules, and a canvas can be exported to a static HTML site or a PDF (both experimental — see [EXPORT.canvas.md](./EXPORT.canvas.md)).

Status: **beta**.

This document is itself a valid meshfox canvas — every `##` section here is a node, nested under this root. See [SPEC.md](./SPEC.md) for more details.

## Concept
<!-- meshfox:node id="concept" -->

- A project is one **canvas**: a tree of nodes starting from a single **root** node.
- From the root, large **section** nodes branch off (e.g. one section per feature).
- From sections, further **block** nodes branch off. Blocks hold Markdown.
- A canvas is stored as a single, plain `.md` file — node metadata (ids, positions, edges) lives in HTML comments, so the file still renders sensibly in GitHub or any plain Markdown viewer, and diffs cleanly in git.
- Markdown inside a block can contain fenced code. A fence can be marked *runnable*; running it executes the code and (optionally) writes the result back into the same node, right under the code, so nobody has to re-run it just to see what happened last time.
- A runnable fence can declare `deps=` on other blocks — running it runs its whole dependency chain first, `make`-style, with nothing run twice. Blocks can also share document-scoped config values (declared once as `meshfox:var`), each block opting in individually via its own `env=` attribute — so running one block never prompts for a variable only some *other* block needs. A `` ```button `` fence is the same `deps=`/`name=`/`default=` machinery with no real code of its own — a prominent shortcut button, captioned by its own body (and, via `name=`/`default=`, a short CLI alias) for a chain that lives elsewhere in the document — see SPEC.md's "Button fences".
- Three ways to interact with the same files:
  - a browser UI (canvas view + block runner) backed by a small Rust server — opens read-only, so pulling up a canvas to look around never one-click-modifies it: running a block is always allowed (you're still explicitly clicking "run", and it's the whole point of a canvas), but a `cache`d block's output isn't written back to the file until an explicit "Edit" button is clicked, which also unlocks dragging, resizing, and saving layout. Output streams into the browser live as the block runs (not just once it's finished), and a running block gets a Kill button, for when one hangs.
  - a CLI that runs blocks non-interactively, `make`-style, for use in scripts/CI — `meshfox list` prints every runnable block as a tree, so there's no need to go spelunking through the file to find out what's runnable. `meshfox run` streams output live too (the same async, killable executor as the browser UI); Ctrl+C kills whichever step is currently running, whole process group and all, and stops there — whatever earlier steps in the chain already completed stays cached.
  - a terminal UI (`meshfox tui`) — the browser's tree-and-block-runner experience without leaving the terminal: browse the node tree, read a node's rendered body (syntax-highlighted code, images), and run blocks with the same live streaming/kill/cache behavior as the other two. See "Terminal UI" in [INTERFACES.canvas.md](./INTERFACES.canvas.md).
- Beyond editing and running: a built-in Starlark constraint system (`meshfox check`) lets a canvas validate its own consistency, and a canvas can be exported to a static HTML site or a PDF (both experimental — see [EXPORT.canvas.md](./EXPORT.canvas.md)).

The web node editor keeps title and Markdown edits in a local draft. **Apply**
(or **Ctrl/Cmd+S**) saves both together and leaves the editor open;
**Save & close** waits for a successful save before closing. **Cancel** discards
changes since the last save. Conflicts keep the draft open for comparison.

## Installation
<!-- meshfox:node id="installation" -->

Per-platform notes for the install options above.

### Docker image
<!-- meshfox:node id="docker-image" -->

`ghcr.io/orofarne/meshfox` runs `meshfox serve --http` (see "Serving canvases over HTTP" in [INTERFACES.canvas.md](./INTERFACES.canvas.md)) on Debian 13, for `linux/amd64` and `linux/arm64`. Mount the directory with your canvases at `/work`:

```sh
docker run --name meshfox --user "$(id -u):$(id -g)" \
  -p 127.0.0.1:4590:4590 -e MESHFOX_TOKEN=change-me \
  -v "$PWD":/work ghcr.io/orofarne/meshfox
```

or, with Python, Node.js and Chromium in the image:

```sh
docker run --name meshfox --user "$(id -u):$(id -g)" \
  -p 127.0.0.1:4590:4590 -e MESHFOX_TOKEN=change-me \
  -v "$PWD":/work ghcr.io/orofarne/meshfox:full
```

Open `http://localhost:4590/` and sign in with the token. Without `MESHFOX_TOKEN` the container generates one and prints it once, in the output above (or in `docker logs meshfox` if you run it detached with `-d`). Publish the port on `127.0.0.1` unless you mean to expose it, and read the security notes above first: the token is a shell on the container.

- **Tags.** `slim` (the default: `latest`, `X.Y.Z`, `X.Y`, `slim`) is meshfox, `bash`, `git`, `curl`, DuckDB (for `display="table"`) and `tini`. `full` (`X.Y.Z-full`, `X.Y-full`, `full`) adds Python 3 with `venv` and `pip`, Node.js with `npm`, and Chromium (for `meshfox pdf`; its sandbox is off in the image, the container is the boundary: `MESHFOX_CHROME_NO_SANDBOX=1`). A pre-release gets only its exact tags.
- **User.** `--user "$(id -u):$(id -g)"` makes blocks write files as you. Without it the container runs as uid 1000. Nothing in the image is world-writable: the home directory (`/tmp/meshfox-home`, which holds `~/.meshfox` — config, plaintext secrets, session state) and the coordinator's socket (`/tmp/meshfox/`) are created at start by whichever uid runs it, so mount a volume at `/tmp/meshfox-home` if that state should outlive the container.
- **Driving it from the host.** `MESHFOX_SERVER_SOCKET` is set inside the image, so `docker exec -it meshfox meshfox tui /work/notes.canvas.md`, `… run …` and `… cores ls` talk to the `serve` that is already running instead of starting their own. Paths are the container's (`/work/...`). Mount the directory at the path you use on the host if you want the two to match.
- **Opening canvases.** `meshfox view` / `cores open` inside the container print the address (`meshfox serve: open http://localhost:4590/c/<slug>/`), since there is no browser. Pass `--public-url` (after the image name) when you reach it by another address.
- **Behind a reverse proxy,** add `--trust-proxy` and `--allowed-host` to the command: `docker run … ghcr.io/orofarne/meshfox serve --http 0.0.0.0:4590 --root /work --trust-proxy --allowed-host notes.example`.
- **Updating** means pulling a newer image and recreating the container: `meshfox check-updates` says so instead of replacing the binary.

The image wraps the binary from the GitHub release of the same tag. To build one yourself: `docker build -f docker/Dockerfile --build-arg MESHFOX_VERSION=0.27.0 --target slim -t meshfox:slim docker/` (or `--target full`); `docker/smoke.sh <image> slim|full <version>` runs the checks CI runs before it pushes. The Debian base has to be at least as new as the release runners' glibc (2.39), which is why it is 13 (trixie) and not 12. `.github/workflows/docker.yml` builds both variants natively on both architectures and publishes the multi-arch manifests after a release; run it by hand with `push` off for a dry run.

### Windows (WSL2)
<!-- meshfox:node id="windows-wsl2" -->

There is no native Windows build; meshfox runs on Windows inside WSL2, as the Linux binary. [`scripts/meshfox-wsl.ps1`](./scripts/meshfox-wsl.ps1) does the setup and starts `meshfox serve --http` (see "Serving canvases over HTTP" in [INTERFACES.canvas.md](./INTERFACES.canvas.md)) for you. In PowerShell:

```powershell
.\scripts\meshfox-wsl.ps1 -CanvasDir D:\notes
```

On every run it:

1. checks that WSL and the distribution (`-Distro`, default `Ubuntu-24.04`) are installed, and installs the distribution if not (finish its first-run setup, then run the script again);
2. installs meshfox into `~/.local/bin` inside the distribution with `scripts/install.sh`, or runs `meshfox check-updates -y` if it is already there (`-SkipUpdate` skips the check);
3. installs the DuckDB CLI (for `display="table"`) with its official installer, which always puts it in `~/.duckdb/cli/<version>/`, and passes that path to meshfox as `MESHFOX_DUCKDB`; `-DuckDbVersion 2.0.0` pins a version instead of the latest stable;
4. creates an access token once and keeps it in `%LOCALAPPDATA%\meshfox\token`, so the sign-in does not change between runs;
5. runs `meshfox serve --http 127.0.0.1:4590 --root <canvas dir>` in the foreground and opens `http://localhost:4590/` in the Windows browser (`-Port` changes the port, `-NoBrowser` skips opening it). Ctrl+C stops it; while the window is open WSL does not shut the distribution down.

- **Distribution.** The release binary needs glibc 2.39 or newer: Ubuntu 24.04 or Debian 13, not Ubuntu 22.04 or Debian 12. It is the same binary as on any Linux, so nothing is rebuilt for WSL.
- **Canvases on the Windows drive.** `-CanvasDir` is a Windows path, reached as `/mnt/c/...` (or `/mnt/d/...`) from WSL, so the files can be opened from Windows tools too. That filesystem is much slower for tree walks than the distribution's own (see "Serving canvases over HTTP" in [INTERFACES.canvas.md](./INTERFACES.canvas.md); add a `.meshfoxignore` for big trees), and file-change events from Windows-side edits may not reach the watchers. For many files, keep the canvases inside the distribution and open them from Windows as `\\wsl$\<distro>\...` instead.
- **Socket.** The coordinator's socket is `~/.meshfox/serve.sock` inside the distribution, not on `/mnt/c`, where unix sockets and mode `0600` are unreliable. To drive it from a WSL shell (`meshfox tui`, `cores ls`, `run`), set `MESHFOX_SERVER_SOCKET=$HOME/.meshfox/serve.sock` there.
- **Not available.** There is no systemd in this setup (the script runs `serve` itself), the macOS menu-bar app is macOS only, and `secret_store = "keychain"` needs a Secret Service (`gnome-keyring`, `dbus-user-session`) running in the distribution; without one, keep the default `plaintext`.

## Documentation
<!-- meshfox:node id="documentation" -->

The rest of the documentation lives in canvases of its own, each a `file` node below:

### Reference
<!-- meshfox:node id="doc-reference" type="file" -->

[REFERENCE.canvas.md](./REFERENCE.canvas.md)

### CLI
<!-- meshfox:node id="doc-cli" type="file" -->

[CLI.canvas.md](./CLI.canvas.md)

### Interfaces
<!-- meshfox:node id="doc-interfaces" type="file" -->

[INTERFACES.canvas.md](./INTERFACES.canvas.md)

### Export
<!-- meshfox:node id="doc-export" type="file" -->

[EXPORT.canvas.md](./EXPORT.canvas.md)

### How-to
<!-- meshfox:node id="doc-how-to" type="file" -->

[HOWTO.canvas.md](./HOWTO.canvas.md)

### Architecture
<!-- meshfox:node id="doc-architecture" type="file" -->

[ARCHITECTURE.canvas.md](./ARCHITECTURE.canvas.md)

### Development
<!-- meshfox:node id="doc-development" type="file" -->

[DEVELOPMENT.canvas.md](./DEVELOPMENT.canvas.md)

## License
<!-- meshfox:node id="license" -->

meshfox is MIT-licensed.

### LICENSE
<!-- meshfox:node id="license-file" type="file" display="code" -->

[LICENSE](./LICENSE)

### Dependency licenses
<!-- meshfox:node id="license-deps" type="file" -->

[LICENSE.canvas.md](./LICENSE.canvas.md)
