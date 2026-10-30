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
#   sh install.sh --no-modify-path
#
# What it does:
#   1. Detects OS + architecture, picks the right release tarball.
#   2. Downloads spid3r-<target>.tar.gz from GitHub releases.
#   3. Installs the `spid3r` binary into $SPID3R_INSTALL_DIR (default ~/.spid3r/bin).
#   4. Installs the standard library into ~/.spid3r/stdlib.
#   5. Adds that directory to your PATH in ~/.bashrc / ~/.zshrc / ~/.profile
#      (only if not already present, skippable via --no-modify-path).
# ============================================================================
set -eu

REPO="needyamin/spid3r"
DOWNLOAD_BASE="https://github.com/${REPO}/releases"
RAW_BASE="https://raw.githubusercontent.com/${REPO}/main"

VERSION=""                 # empty = latest release
FROM_SOURCE=0
FEATURES=""                # empty = auto-detect (source builds only)
CORE_ONLY=0
NO_MODIFY_PATH=0
INSTALL_DIR="${SPID3R_INSTALL_DIR:-$HOME/.spid3r/bin}"

say()  { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

show_help() {
    cat <<'EOF'
Spid3r installer for Linux and macOS.

Usage:
  curl -fsSL https://raw.githubusercontent.com/needyamin/spid3r/main/install.sh | sh
  sh install.sh [OPTIONS]

Options:
  --version <ver>         Install a specific release version (e.g. 1.0.0; default: latest)
  --dir <path>            Target directory for the binary (default: ~/.spid3r/bin)
  --source                Compile and build from source using cargo
  --core                  Source build with zero external dependencies (no LLVM/WebKit)
  --features <feat>       Comma-separated Cargo features (e.g. native,gui)
  --no-modify-path        Do not append PATH exports to shell profiles (~/.bashrc, ~/.zshrc, etc.)
  -h, --help              Show this help message
EOF
}

# ---------------------------------------------------------------- arguments --
while [ $# -gt 0 ]; do
    case "$1" in
        --version)          [ $# -ge 2 ] || die "--version needs a value (e.g. 1.0.0)"
                            VERSION="$2"; shift 2 ;;
        --dir)              [ $# -ge 2 ] || die "--dir needs a value"
                            INSTALL_DIR="$2"; shift 2 ;;
        --source)           FROM_SOURCE=1; shift ;;
        --features)         [ $# -ge 2 ] || die "--features needs a value (e.g. native,gui)"
                            FEATURES="$2"; shift 2 ;;
        --core)             CORE_ONLY=1; shift ;;
        --no-modify-path)   NO_MODIFY_PATH=1; shift ;;
        -h|--help)          show_help; exit 0 ;;
        *)                  die "unknown option: $1 (try --help)" ;;
    esac
done

# ------------------------------------------------------------------- checks --
command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 \
    || die "curl or wget is required to download the installer payload"

command -v tar >/dev/null 2>&1 \
    || die "tar is required to extract the installer payload"

fetch() { # fetch <url> <outfile>
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    else
        wget -qO "$2" "$1"
    fi
}

install_stdlib_tree() {
    src_stdlib="$1"
    dst_parent="$(dirname "$INSTALL_DIR")"
    if [ -d "$src_stdlib" ]; then
        mkdir -p "${dst_parent}/stdlib"
        cp -R "$src_stdlib"/* "${dst_parent}/stdlib/" 2>/dev/null || true
        mkdir -p "${INSTALL_DIR}/stdlib"
        cp -R "$src_stdlib"/* "${INSTALL_DIR}/stdlib/" 2>/dev/null || true
        say "Standard library installed to ${dst_parent}/stdlib"
        return 0
    fi
    return 1
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
    PARENT_DIR="$(dirname "$INSTALL_DIR")"
    mkdir -p "$INSTALL_DIR"

    if [ -n "$FEATURES" ]; then
        say "Features: ${FEATURES}"
        cargo install --locked --git "https://github.com/${REPO}.git" spid3r \
            --root "$PARENT_DIR" --no-default-features --features "$FEATURES"
    else
        say "Features: core only (VM + interpreter, zero dependencies)"
        cargo install --locked --git "https://github.com/${REPO}.git" spid3r \
            --root "$PARENT_DIR" --no-default-features
    fi

    if [ -f "${PARENT_DIR}/bin/spid3r" ] && [ "${INSTALL_DIR}" != "${PARENT_DIR}/bin" ]; then
        cp -f "${PARENT_DIR}/bin/spid3r" "${INSTALL_DIR}/spid3r"
    fi

    # Fetch stdlib if not already present
    if [ ! -d "${PARENT_DIR}/stdlib" ]; then
        say "Fetching standard library modules..."
        TMPDIR_DL="$(mktemp -d 2>/dev/null || mktemp -d -t spid3r)"
        if fetch "https://github.com/${REPO}/archive/refs/heads/main.tar.gz" "${TMPDIR_DL}/repo.tar.gz" 2>/dev/null; then
            tar -xzf "${TMPDIR_DL}/repo.tar.gz" -C "$TMPDIR_DL" 2>/dev/null || true
            STDLIB_FOUND="$(find "$TMPDIR_DL" -maxdepth 3 -type d -name stdlib | head -n 1)"
            if [ -n "$STDLIB_FOUND" ] && [ -d "$STDLIB_FOUND" ]; then
                install_stdlib_tree "$STDLIB_FOUND"
            fi
        fi
        rm -rf "$TMPDIR_DL"
    fi

    say "Done. spid3r is installed into ${INSTALL_DIR}."
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
    Linux)
        libc="unknown-linux-gnu" # release tarballs are glibc builds
        if (ldd --version 2>&1 | grep -qi "musl") || [ -f /lib/ld-musl-x86_64.so.1 ] || [ -f /lib/ld-musl-aarch64.so.1 ]; then
            warn "Detected musl-based Linux (e.g. Alpine). Official prebuilt binaries require glibc."
            warn "If running the prebuilt binary fails, install via source: sh install.sh --source --core"
        fi
        ;;
    Darwin)
        libc="apple-darwin"
        ;;
    *)
        die "unsupported OS: $OS (this installer covers Linux and macOS)"
        ;;
esac

TARGET="${arch}-${libc}"
say "Detected target: ${TARGET}"

# --------------------------------------------------------------- download ----
if [ -n "$VERSION" ]; then
    TARBALL_URL="${DOWNLOAD_BASE}/download/v${VERSION}/spid3r-${TARGET}.tar.gz"
else
    TARBALL_URL="${DOWNLOAD_BASE}/latest/download/spid3r-${TARGET}.tar.gz"
fi

TMPDIR_DL="$(mktemp -d 2>/dev/null || mktemp -d -t spid3r)"
trap 'rm -rf "$TMPDIR_DL"' EXIT INT TERM

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

# ---------------------------------------------------------- stdlib install ---
STDLIB_SRC="$(find "$TMPDIR_DL" -maxdepth 3 -type d -name stdlib | head -n 1)"
if [ -n "$STDLIB_SRC" ] && [ -d "$STDLIB_SRC" ]; then
    install_stdlib_tree "$STDLIB_SRC"
else
    say "Fetching standard library modules..."
    if fetch "https://github.com/${REPO}/archive/refs/heads/main.tar.gz" "${TMPDIR_DL}/repo.tar.gz" 2>/dev/null; then
        tar -xzf "${TMPDIR_DL}/repo.tar.gz" -C "$TMPDIR_DL" 2>/dev/null || true
        STDLIB_SRC="$(find "$TMPDIR_DL" -maxdepth 3 -type d -name stdlib | head -n 1)"
        if [ -n "$STDLIB_SRC" ] && [ -d "$STDLIB_SRC" ]; then
            install_stdlib_tree "$STDLIB_SRC"
        fi
    else
        warn "Could not fetch standard library modules automatically. You can install them by cloning the stdlib folder."
    fi
fi

# --------------------------------------------------------------- PATH set ----
path_added=0
if [ "$NO_MODIFY_PATH" -eq 0 ]; then
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*)
            # Directory is already in PATH
            ;;
        *)
            path_line='# Spid3r (installed by install.sh)'
            export_line="export PATH=\"${INSTALL_DIR}:\$PATH\""

            candidates=""
            for rc in "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.profile"; do
                if [ -f "$rc" ]; then
                    candidates="${candidates} $rc"
                fi
            done

            if [ -z "$candidates" ]; then
                candidates="$HOME/.profile"
                touch "$HOME/.profile"
            fi

            for rc in $candidates; do
                if ! grep -qF "$export_line" "$rc" 2>/dev/null && ! grep -qF "${INSTALL_DIR}" "$rc" 2>/dev/null; then
                    {
                        printf '\n%s\n' "$path_line"
                        printf '%s\n' "$export_line"
                    } >> "$rc"
                    say "PATH updated: added ${INSTALL_DIR} to ${rc}"
                    path_added=1
                fi
            done
            ;;
    esac
fi

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
