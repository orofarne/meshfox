#!/bin/sh
# meshfox installer — downloads the latest (or a pinned) GitHub release
# binary for this machine and puts it on PATH.
#
#   curl -fsSL https://raw.githubusercontent.com/orofarne/meshfox/main/scripts/install.sh | sh
#
# Non-interactive / CI use:
#   curl -fsSL .../install.sh | sh -s -- -y
#
# Options (pass after `--` when piping through curl):
#   -y, --yes            never prompt; assume "yes" to any PATH change
#   --no-modify-path      never touch shell rc files, whatever else is passed
#   --install-dir DIR     install the binary here (default: $HOME/.local/bin)
#   --version TAG          install a specific release tag (default: latest)
#   --target TRIPLE         override OS/arch autodetection

set -eu

REPO="orofarne/meshfox"
BIN_NAME="meshfox"
INSTALL_DIR="${HOME}/.local/bin"
INSTALL_DIR_EXPLICIT=false
VERSION="latest"
TARGET=""
ASSUME_YES=false
MODIFY_PATH=true

MASCOT='
 /\_/\
( ¬‿¬ )──●──●──●
  c c
'

err() {
    echo "meshfox-install: error: $*" >&2
    exit 1
}

info() {
    echo "meshfox-install: $*"
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || err "'$1' is required but not found on PATH"
}

usage() {
    cat <<'EOF'
Usage: install.sh [options]

  -y, --yes             never prompt; assume "yes" to any PATH change
  --no-modify-path       never touch shell rc files
  --install-dir DIR      install location (default: $HOME/.local/bin)
  --version TAG           release tag to install (default: latest)
  --target TRIPLE         target triple, e.g. x86_64-unknown-linux-gnu
  -h, --help             show this help
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        -y | --yes) ASSUME_YES=true ;;
        --no-modify-path) MODIFY_PATH=false ;;
        --install-dir)
            [ $# -ge 2 ] || err "--install-dir requires an argument"
            INSTALL_DIR="$2"
            INSTALL_DIR_EXPLICIT=true
            shift
            ;;
        --version)
            [ $# -ge 2 ] || err "--version requires an argument"
            VERSION="$2"
            shift
            ;;
        --target)
            [ $# -ge 2 ] || err "--target requires an argument"
            TARGET="$2"
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) err "unrecognized option '$1' (see --help)" ;;
    esac
    shift
done

detect_target() {
    if [ -n "$TARGET" ]; then
        return
    fi

    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux) os_part="unknown-linux-gnu" ;;
        Darwin) os_part="apple-darwin" ;;
        *) err "unsupported OS '$os' — meshfox ships prebuilt Linux and macOS binaries only" ;;
    esac

    case "$arch" in
        x86_64 | amd64) arch_part="x86_64" ;;
        arm64 | aarch64) arch_part="aarch64" ;;
        *) err "unsupported architecture '$arch'" ;;
    esac

    TARGET="${arch_part}-${os_part}"

    case "$TARGET" in
        x86_64-unknown-linux-gnu | x86_64-apple-darwin | aarch64-apple-darwin) ;;
        *) err "no prebuilt binary for '$TARGET' — see https://github.com/${REPO}/releases" ;;
    esac
}

download() {
    src="$1"
    dest="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$src" -o "$dest"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$src" -O "$dest"
    else
        err "need either 'curl' or 'wget' to download meshfox"
    fi
}

prompt_install_dir() {
    [ "$INSTALL_DIR_EXPLICIT" = false ] || return 0
    [ "$ASSUME_YES" = false ] || return 0
    [ -t 0 ] && [ -t 1 ] || return 0

    printf 'meshfox-install: install directory? [%s] ' "$INSTALL_DIR"
    read -r reply || reply=""
    [ -n "$reply" ] && INSTALL_DIR="$reply"
}

find_existing() {
    existing_bin=""
    if command -v "$BIN_NAME" >/dev/null 2>&1; then
        existing_bin="$(command -v "$BIN_NAME")"
    elif [ -x "${INSTALL_DIR}/${BIN_NAME}" ]; then
        existing_bin="${INSTALL_DIR}/${BIN_NAME}"
    fi
}

offer_existing() {
    find_existing
    [ -n "$existing_bin" ] || return 0

    existing_version="$("$existing_bin" --version 2>/dev/null || echo "unknown version")"
    info "found an existing install: ${existing_bin} (${existing_version})"

    [ "$ASSUME_YES" = false ] || return 0
    [ -t 0 ] && [ -t 1 ] || return 0

    echo "  1) self-update it in place (${BIN_NAME} check-updates)"
    echo "  2) install a fresh copy anyway"
    echo "  3) exit"
    while true; do
        printf 'Choice [1/2/3]: '
        read -r reply || reply=""
        case "$reply" in
            1) exec "$existing_bin" check-updates -y ;;
            2) return 0 ;;
            3) err "installation aborted" ;;
            *) echo "please enter 1, 2, or 3" ;;
        esac
    done
}

confirm_install() {
    printf '%s\n' "$MASCOT"
    echo "meshfox installer"
    echo "------------------"
    echo "Version:      ${VERSION}"
    echo "Target:       ${TARGET}"
    echo "Install path: ${INSTALL_DIR}"
    echo "------------------"

    [ "$ASSUME_YES" = false ] || return 0
    [ -t 0 ] && [ -t 1 ] || return 0

    printf 'Proceed with installation? [Y/n] '
    read -r reply || reply=""
    case "$reply" in
        [nN]*) err "installation aborted" ;;
        *) ;;
    esac
}

detect_target
need_cmd tar
need_cmd mktemp

offer_existing
prompt_install_dir

archive="${BIN_NAME}-${TARGET}.tar.gz"
if [ "$VERSION" = "latest" ]; then
    url="https://github.com/${REPO}/releases/latest/download/${archive}"
else
    url="https://github.com/${REPO}/releases/download/${VERSION}/${archive}"
fi

confirm_install

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

info "downloading ${archive} (${VERSION})"
download "$url" "${workdir}/${archive}"

tar xzf "${workdir}/${archive}" -C "$workdir"

bin_src="${workdir}/${TARGET}/${BIN_NAME}"
[ -f "$bin_src" ] || err "downloaded archive didn't contain ${TARGET}/${BIN_NAME}"

mkdir -p "$INSTALL_DIR"
install -m 755 "$bin_src" "${INSTALL_DIR}/${BIN_NAME}"
info "installed ${BIN_NAME} to ${INSTALL_DIR}/${BIN_NAME}"

path_contains_dir() {
    case ":${PATH}:" in
        *":${1}:"*) return 0 ;;
        *) return 1 ;;
    esac
}

rcfile_for_shell() {
    case "$(basename "${SHELL:-sh}")" in
        zsh) echo "${ZDOTDIR:-$HOME}/.zshrc" ;;
        bash) echo "${HOME}/.bashrc" ;;
        fish) echo "${HOME}/.config/fish/config.fish" ;;
        *) echo "${HOME}/.profile" ;;
    esac
}

add_path_line() {
    rcfile="$1"
    marker="# added by meshfox-install"
    if [ -f "$rcfile" ] && grep -qF "$marker" "$rcfile" 2>/dev/null; then
        return
    fi
    mkdir -p "$(dirname "$rcfile")"
    {
        echo ""
        echo "$marker"
        case "$rcfile" in
            *fish*) echo "fish_add_path \"$INSTALL_DIR\"" ;;
            *) echo "export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
        esac
    } >>"$rcfile"
}

if path_contains_dir "$INSTALL_DIR"; then
    :
elif [ "$MODIFY_PATH" = false ]; then
    info "${INSTALL_DIR} is not on PATH — add it yourself to run '${BIN_NAME}' directly"
else
    rcfile="$(rcfile_for_shell)"
    do_add=false
    if [ "$ASSUME_YES" = true ]; then
        do_add=true
    elif [ -t 0 ] && [ -t 1 ]; then
        printf 'meshfox-install: add %s to PATH by modifying %s? [Y/n] ' "$INSTALL_DIR" "$rcfile"
        read -r reply || reply=""
        case "$reply" in
            [nN]*) do_add=false ;;
            *) do_add=true ;;
        esac
    else
        info "${INSTALL_DIR} is not on PATH; run with -y to add it to ${rcfile} non-interactively"
    fi

    if [ "$do_add" = true ]; then
        add_path_line "$rcfile"
        info "added ${INSTALL_DIR} to PATH in ${rcfile} — restart your shell or run 'source ${rcfile}'"
    fi
fi

info "run '${BIN_NAME} --help' to get started"
