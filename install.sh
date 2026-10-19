#!/bin/sh
# ============================================================================
# Spid3r installer for Linux (and macOS).
#
#   curl -fsSL https://raw.githubusercontent.com/needyamin/spid3r/main/install.sh | sh
#
# or, with options:
#
#   sh install.sh --version 1.0.0
#   sh install.sh --dir /usr/local/bin
#   sh install.sh --source        # build from source via cargo instead
#   sh install.sh --source --core # source build, no LLVM/WebKit needed
#   sh install.sh --source --features native,gui
#
# What it does:
#   1. Detects OS + architecture, picks the right release tarball.
#   2. Downloads spid3r-<target>.tar.gz from GitHub releases.
#   3. Installs the `spid3r` binary into $SPID3R_INSTALL_DIR (default ~/.spid3r/bin).
#   4. Adds that directory to your PATH in ~/.bashrc / ~/.zshrc / ~/.profile
#      (only if not already present).
# ============================================================================
set -eu

REPO="needyamin/spid3r"
DOWNLOAD_BASE="https://github.com/${REPO}/releases"
RAW_BASE="https://raw.githubusercontent.com/${REPO}/main"

VERSION=""                 # empty = latest release
FROM_SOURCE=0
FEATURES=""                # empty = auto-detect (source builds only)
CORE_ONLY=0
INSTALL_DIR="${SPID3R_INSTALL_DIR:-$HOME/.spid3r/bin}"

say()  { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------- arguments --
while [ $# -gt 0 ]; do
    case "$1" in
        --version)  [ $# -ge 2 ] || die "--version needs a value (e.g. 1.0.0)"
                    VERSION="$2"; shift 2 ;;
        --dir)      [ $# -ge 2 ] || die "--dir needs a value"
                    INSTALL_DIR="$2"; shift 2 ;;
        --source)   FROM_SOURCE=1; shift ;;
        --features) [ $# -ge 2 ] || die "--features needs a value (e.g. native,gui)"
                    FEATURES="$2"; shift 2 ;;
        --core)     CORE_ONLY=1; shift ;;
        -h|--help)  sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)          die "unknown option: $1 (try --help)" ;;
    esac
done

# ------------------------------------------------------------------- checks --
command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 \
    || die "curl or wget is required to download the installer payload"

fetch() { # fetch <url> <outfile|- >
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    else
        wget -qO "$2" "$1"
    fi
}

# ------------------------------------------------------ build from source ----
if [ "$FROM_SOURCE" -eq 1 ]; then
    command -v cargo >/dev/null 2>&1 \
        || die "cargo not found. Install Rust first: https://rustup.rs"
    command -v cc >/dev/null 2>&1 || command -v clang >/dev/null 2>&1 \
        || die "a C toolchain is required (Debian/Ubuntu: sudo apt install build-essential)"

    # The default features (gui + native) need system libraries. Enable only
    # the ones that are actually installed so the build does not fail.
    if [ -z "$FEATURES" ] && [ "$CORE_ONLY" -eq 0 ]; then
        have_llvm=0; have_webkit=0
        for c in llvm-config llvm-config-18 llvm-config-19 /usr/lib/llvm-18/bin/llvm-config; do
            if command -v "$c" >/dev/null 2>&1; then have_llvm=1; break; fi
        done
        if command -v pkg-config >/dev/null 2>&1 \
            && pkg-config --exists webkit2gtk-4.1 2>/dev/null; then have_webkit=1; fi
        [ "$have_llvm" -eq 1 ]   && FEATURES="native"
        [ "$have_webkit" -eq 1 ] && FEATURES="${FEATURES:+$FEATURES,}gui"
        [ "$have_llvm" -eq 1 ]   || warn "LLVM 18 not found: skipping the native backend (sudo apt install llvm-18-dev)"
        [ "$have_webkit" -eq 1 ] || warn "WebKit2GTK 4.1 not found: skipping 'spid3r gui' (sudo apt install libwebkit2gtk-4.1-dev)"
    fi

    say "Building spid3r from source (this compiles the whole toolchain)..."
    if [ -n "$FEATURES" ]; then
        say "Features: ${FEATURES}"
        cargo install --locked --git "https://github.com/${REPO}.git" spid3r \
            --no-default-features --features "$FEATURES"
    else
        say "Features: core only (VM + interpreter, zero dependencies)"
        cargo install --locked --git "https://github.com/${REPO}.git" spid3r \
            --no-default-features
    fi
    say "Done. spid3r is installed into ~/.cargo/bin (make sure it is on PATH)."
    exit 0
fi

# ------------------------------------------------------------ detect target --
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$ARCH" in
    x86_64|amd64)  arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *) die "unsupported architecture: $ARCH" ;;
esac

case "$OS" in
    Linux)  libc="unknown-linux-gnu" ;;    # release tarballs are glibc builds
    Darwin) libc="apple-darwin" ;;
    *) die "unsupported OS: $OS (this installer covers Linux and macOS)" ;;
esac

TARGET="${arch}-${libc}"
say "Detected target: ${TARGET}"

# --------------------------------------------------------------- download ----
if [ -n "$VERSION" ]; then
    TARBALL_URL="${DOWNLOAD_BASE}/download/v${VERSION}/spid3r-${TARGET}.tar.gz"
else
    TARBALL_URL="${DOWNLOAD_BASE}/latest/download/spid3r-${TARGET}.tar.gz"
fi

TMPDIR_DL="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_DL"' EXIT

say "Downloading ${TARBALL_URL}"
fetch "$TARBALL_URL" "${TMPDIR_DL}/spid3r.tar.gz" \
    || die "download failed (no release asset for ${TARGET}? attach spid3r-${TARGET}.tar.gz to a release, or use --source)"

tar -xzf "${TMPDIR_DL}/spid3r.tar.gz" -C "$TMPDIR_DL" \
    || die "failed to extract the tarball"

# Locate the binary inside the extracted tree (tolerates a wrapping dir).
BIN_SRC="$(find "$TMPDIR_DL" -type f -name spid3r | head -n 1)"
[ -n "$BIN_SRC" ] || die "tarball did not contain a 'spid3r' binary"

# ----------------------------------------------------------------- install ---
mkdir -p "$INSTALL_DIR"
chmod +x "$BIN_SRC"
mv -f "$BIN_SRC" "${INSTALL_DIR}/spid3r"

# --------------------------------------------------------------- PATH set ----
path_line='# Spid3r (installed by install.sh)'
export_line="export PATH=\"${INSTALL_DIR}:\$PATH\""

path_added=0
for rc in "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.profile"; do
    [ -f "$rc" ] || touch "$rc"
    if ! grep -qF "$export_line" "$rc" && ! grep -qF "${INSTALL_DIR}" "$rc"; then
        {
            printf '\n%s\n' "$path_line"
            printf '%s\n' "$export_line"
        } >> "$rc"
        say "PATH updated: added ${INSTALL_DIR} to ${rc}"
        path_added=1
    fi
done

# -------------------------------------------------------------- verify -------
if [ -x "${INSTALL_DIR}/spid3r" ]; then
    say ""
    say "Spid3r installed successfully:"
    "${INSTALL_DIR}/spid3r" --version || true
else
    die "installation finished but the binary is not executable at ${INSTALL_DIR}/spid3r"
fi

say ""
if [ "$path_added" -eq 1 ]; then
    say "Open a new terminal (or run: source ~/.bashrc) and then try:"
else
    say "PATH already configured. Try:"
fi
say "    spid3r --help"
say "    spid3r run hello.matrix"
