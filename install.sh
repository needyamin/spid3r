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
#   sh install.sh --modify-path   # non-interactively add PATH even on a TTY
#   sh install.sh --no-modify-path # skip PATH edits (or SPID3R_NO_PATH=1)
#   sh install.sh --no-editor-ext # skip IDE extension install (or SPID3R_NO_EDITOR_EXT=1)
#   sh install.sh --uninstall     # remove Spid3r binaries, symlinks, libs, and PATH edits
#
# What it does:
#   1. Detects OS + architecture, picks the right release tarball.
#   2. Downloads spid3r-<target>.tar.gz from GitHub releases.
#   3. Atomically installs `spid3r` (+ `spid3r-self`, `sp`, `yamc`, `yamcvm`,
#      `spvm`, `spkg`, and bundled libs) into $SPID3R_INSTALL_DIR (default
#      ~/.spid3r/bin) so running binaries are replaced cleanly without ETXTBSY.
#   4. Removes a `stdlib/` directory left by an older version of this script
#      (the standard library is compiled into the binary now).
#   5. Adds that directory and the spkg tool bin directory
#      (${XDG_STATE_HOME:-$HOME/.local/state}/spid3r/bin) to your PATH in
#      ~/.bashrc / ~/.zshrc / ~/.profile (prompts interactively on a TTY
#      unless --modify-path / --no-modify-path / SPID3R_NO_PATH=1 is set).
#   6. Verifies and installs the bundled spid3r-formatter.vsix extension into any
#      detected VS Code / Cursor / Windsurf / VSCodium CLI on PATH (offline,
#      skippable via --no-editor-ext or SPID3R_NO_EDITOR_EXT=1).
#
# The standard library is not installed as files. Every stdlib module is
# compiled into the `spid3r` binary, and all backends (tree walker, VM, AOT)
# fall back to that table, so `import "json"` works with the binary alone.
# ============================================================================
set -eu

REPO="needyamin/spid3r"
DOWNLOAD_BASE="https://github.com/${REPO}/releases"
RAW_BASE="https://raw.githubusercontent.com/${REPO}/main"

VERSION=""                 # empty = latest release
FROM_SOURCE=0
FEATURES=""                # empty = auto-detect (source builds only)
CORE_ONLY=0
MODIFY_PATH_OPT=0
NO_MODIFY_PATH=0
if [ "${SPID3R_NO_PATH:-0}" = "1" ]; then
    NO_MODIFY_PATH=1
fi
INSTALL_EDITOR_EXT=1
if [ "${SPID3R_NO_EDITOR_EXT:-0}" = "1" ]; then
    INSTALL_EDITOR_EXT=0
fi
DO_UNINSTALL=0
INSTALL_DIR="${SPID3R_INSTALL_DIR:-}"

SCRIPT_DIR=""
case "$0" in
    */*)
        if [ -f "$0" ]; then
            SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
        fi
        ;;
esac

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
  --version <ver>         Install a specific release version (e.g. 1.5.0; default: latest)
  --dir <path>            Target directory for binaries (default: ~/.spid3r/bin)
  --source                Compile and build from source using cargo
  --core                  Source build with zero external dependencies (no LLVM/WebKit)
  --features <feat>       Comma-separated Cargo features (e.g. native,gui)
  --modify-path, -y, --yes
                          Add install and spkg tool bin directories to shell PATH without prompting
  --no-modify-path        Do not append PATH exports to shell profiles (or set SPID3R_NO_PATH=1)
  --editor-ext            Install bundled IDE formatter extension into detected editors (default)
  --no-editor-ext         Skip IDE formatter extension install (or set SPID3R_NO_EDITOR_EXT=1)
  --uninstall             Remove installed Spid3r binaries, symlinks, bundled libs, and PATH edits
  -h, --help              Show this help message
EOF
}

# ---------------------------------------------------------------- arguments --
while [ $# -gt 0 ]; do
    case "$1" in
        --version)          [ $# -ge 2 ] || die "--version needs a value (e.g. 1.5.0)"
                            VERSION="$2"; shift 2 ;;
        --dir)              [ $# -ge 2 ] && [ -n "$2" ] || die "--dir needs a value"
                            INSTALL_DIR="$2"; shift 2 ;;
        --source)           FROM_SOURCE=1; shift ;;
        --features)         [ $# -ge 2 ] || die "--features needs a value (e.g. native,gui)"
                            FEATURES="$2"; shift 2 ;;
        --core)             CORE_ONLY=1; shift ;;
        --modify-path|-y|--yes)
                            MODIFY_PATH_OPT=1; NO_MODIFY_PATH=0; shift ;;
        --no-modify-path)   NO_MODIFY_PATH=1; MODIFY_PATH_OPT=0; shift ;;
        --editor-ext)       INSTALL_EDITOR_EXT=1; shift ;;
        --no-editor-ext)    INSTALL_EDITOR_EXT=0; shift ;;
        --uninstall)        DO_UNINSTALL=1; shift ;;
        -h|--help)          show_help; exit 0 ;;
        *)                  die "unknown option: $1 (try --help)" ;;
    esac
done

strip_trailing_slashes() { # strip_trailing_slashes <path>
    sts_p="$1"
    while [ "$sts_p" != "/" ]; do
        case "$sts_p" in
            */) sts_p="${sts_p%/}" ;;
            *)  break ;;
        esac
    done
    printf '%s\n' "$sts_p"
}

HOME_NORM="$(strip_trailing_slashes "${HOME:-}")"
if [ "$HOME_NORM" = "/" ]; then
    HOME_PREFIX=""
else
    HOME_PREFIX="$HOME_NORM"
fi
if [ -z "$INSTALL_DIR" ]; then
    INSTALL_DIR="${HOME_PREFIX}/.spid3r/bin"
else
    INSTALL_DIR="$(strip_trailing_slashes "$INSTALL_DIR")"
fi
STATE_BASE="$(strip_trailing_slashes "${XDG_STATE_HOME:-${HOME_PREFIX}/.local/state}")"
if [ "$STATE_BASE" = "/" ]; then
    TOOL_BIN_DIR="/spid3r/bin"
else
    TOOL_BIN_DIR="${STATE_BASE}/spid3r/bin"
fi

fetch() { # fetch <url> <outfile>
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    else
        wget -qO "$2" "$1"
    fi
}

# Stage in the destination directory and rename(2) atomically so upgrading
# over running `spid3r` or mapped shared libs never triggers ETXTBSY / SIGBUS.
atomic_install_file() { # atomic_install_file <src> <dst> <mode>
    aif_src="$1"
    aif_dst="$2"
    aif_mode="$3"
    aif_dir="$(dirname "$aif_dst")"
    aif_tmp="${aif_dir}/.$(basename "$aif_dst").new.$$"
    mkdir -p "$aif_dir"
    if cp -L -f "$aif_src" "$aif_tmp" && chmod "$aif_mode" "$aif_tmp" && mv -f "$aif_tmp" "$aif_dst"; then
        return 0
    fi
    rm -f "$aif_tmp"
    return 1
}

lib_dir_for_install() {
    case "$(basename "$INSTALL_DIR")" in
        bin) printf '%s\n' "$(dirname "$INSTALL_DIR")/lib" ;;
        *)   printf '%s\n' "${INSTALL_DIR}/lib" ;;
    esac
}

# Remove a `stdlib/` directory left behind by an older version of this script.
remove_legacy_stdlib() {
    for dir in "${INSTALL_DIR}/stdlib" "$(dirname "$INSTALL_DIR")/stdlib"; do
        [ -d "$dir" ] || continue
        has_matrix=0
        for f in "$dir"/*.matrix; do
            if [ -e "$f" ]; then
                has_matrix=1
                break
            fi
        done
        if [ "$has_matrix" -eq 1 ]; then
            rm -rf "$dir"
            say "Removed legacy standard library (now built in): ${dir}"
        fi
    done
}

verify_sha256_file() { # verify_sha256_file <file> <sha256_file>
    vsf_file="$1"
    vsf_sha="$2"
    [ -f "$vsf_file" ] && [ -f "$vsf_sha" ] || return 1
    vsf_expected="$(awk '{print tolower($1); exit}' "$vsf_sha" 2>/dev/null || true)"
    case "$vsf_expected" in
        ''|*[!0-9a-f]*) return 1 ;;
    esac
    [ "${#vsf_expected}" -eq 64 ] || return 1
    if command -v sha256sum >/dev/null 2>&1; then
        vsf_actual="$(sha256sum "$vsf_file" | awk '{print tolower($1); exit}')"
    elif command -v shasum >/dev/null 2>&1; then
        vsf_actual="$(shasum -a 256 "$vsf_file" | awk '{print tolower($1); exit}')"
    else
        return 2
    fi
    [ "$vsf_actual" = "$vsf_expected" ]
}

has_any_editor() {
    for he_name in code cursor windsurf codium; do
        if command -v "$he_name" >/dev/null 2>&1; then
            return 0
        fi
    done
    return 1
}

run_with_timeout() { # run_with_timeout <secs> <cmd...>
    rwt_secs="$1"
    shift
    if command -v timeout >/dev/null 2>&1; then
        timeout "$rwt_secs" "$@"
    elif command -v gtimeout >/dev/null 2>&1; then
        gtimeout "$rwt_secs" "$@"
    else
        "$@"
    fi
}

install_editor_extension() { # install_editor_extension [extracted_tar_dir]
    iee_extract="${1:-}"
    if [ "$INSTALL_EDITOR_EXT" -eq 0 ]; then
        say "Skipping IDE formatter extension (--no-editor-ext / SPID3R_NO_EDITOR_EXT=1)."
        return 0
    fi

    iee_vsix=""
    iee_sha=""
    iee_tmp=""

    # 1. Bundled inside extracted release tarball (100% offline).
    if [ -n "$iee_extract" ] && [ -d "$iee_extract" ]; then
        iee_vsix="$(find "$iee_extract" -type f -name 'spid3r-formatter.vsix' | head -n 1 || true)"
        if [ -n "$iee_vsix" ] && [ -f "${iee_vsix}.sha256" ]; then
            iee_sha="${iee_vsix}.sha256"
        fi
    fi

    # 2. Local repository checkout (e.g. `sh ./install.sh --source`, 100% offline).
    if [ -z "$iee_vsix" ] && [ -n "$SCRIPT_DIR" ] && [ -f "${SCRIPT_DIR}/packaging/vscode/spid3r-formatter.vsix" ]; then
        iee_vsix="${SCRIPT_DIR}/packaging/vscode/spid3r-formatter.vsix"
        if [ -f "${iee_vsix}.sha256" ]; then
            iee_sha="${iee_vsix}.sha256"
        fi
    fi

    # 3. `--source` / older tarball fallback: fetch only when a supported editor is on PATH.
    if [ -z "$iee_vsix" ]; then
        if ! has_any_editor; then
            return 0
        fi
        iee_tmp="$(mktemp -d 2>/dev/null || mktemp -d -t spid3r_vsix)"
        if fetch "${RAW_BASE}/packaging/vscode/spid3r-formatter.vsix" "${iee_tmp}/spid3r-formatter.vsix" 2>/dev/null \
            && fetch "${RAW_BASE}/packaging/vscode/spid3r-formatter.vsix.sha256" "${iee_tmp}/spid3r-formatter.vsix.sha256" 2>/dev/null; then
            iee_vsix="${iee_tmp}/spid3r-formatter.vsix"
            iee_sha="${iee_tmp}/spid3r-formatter.vsix.sha256"
        else
            warn "could not download spid3r-formatter.vsix; skipping IDE extension install."
            rm -rf "$iee_tmp"
            return 0
        fi
    fi

    if [ -z "$iee_sha" ] || [ ! -f "$iee_sha" ]; then
        warn "missing SHA-256 pin for spid3r-formatter.vsix; skipping IDE extension install."
        [ -n "$iee_tmp" ] && rm -rf "$iee_tmp"
        return 0
    fi

    if verify_sha256_file "$iee_vsix" "$iee_sha"; then
        :
    else
        iee_vrc=$?
        if [ "$iee_vrc" -eq 2 ]; then
            warn "neither sha256sum nor shasum found; skipping unverified IDE extension install."
        else
            warn "spid3r-formatter.vsix SHA-256 verification failed; skipping IDE extension install."
        fi
        [ -n "$iee_tmp" ] && rm -rf "$iee_tmp"
        return 0
    fi

    iee_share="$(dirname "$INSTALL_DIR")/share/spid3r"
    iee_dest="$iee_vsix"
    if atomic_install_file "$iee_vsix" "${iee_share}/spid3r-formatter.vsix" 0644 2>/dev/null; then
        iee_dest="${iee_share}/spid3r-formatter.vsix"
    fi

    if [ "$(id -u)" -eq 0 ]; then
        say "Offline IDE formatter VSIX saved to ${iee_dest} (skipping automatic IDE CLI install as root)."
        say "Install for your user with: code --install-extension \"${iee_dest}\""
        [ -n "$iee_tmp" ] && rm -rf "$iee_tmp"
        return 0
    fi

    iee_found=0
    for iee_ed in code cursor windsurf codium; do
        iee_bin="$(command -v "$iee_ed" 2>/dev/null || true)"
        [ -n "$iee_bin" ] || continue
        iee_found=$((iee_found + 1))
        if run_with_timeout 60 "$iee_bin" --install-extension "$iee_dest" --force </dev/null >/dev/null 2>&1; then
            say "Installed Spid3r IDE formatter extension for ${iee_ed} (offline VSIX)."
        else
            warn "${iee_ed} --install-extension failed; install manually via: ${iee_ed} --install-extension \"${iee_dest}\""
        fi
    done

    if [ "$iee_found" -eq 0 ]; then
        say "No supported editor CLI (code, cursor, windsurf, codium) found on PATH."
        say "Offline IDE formatter extension saved to: ${iee_dest}"
    fi

    [ -n "$iee_tmp" ] && rm -rf "$iee_tmp"
    return 0
}

path_added=0
path_already=0
path_in_rc=0

write_rc_inplace() { # write_rc_inplace <tmp_file> <rc_file>
    wri_tmp="$1"
    wri_rc="$2"
    if [ -w "$wri_rc" ] && cat "$wri_tmp" > "$wri_rc"; then
        rm -f "$wri_tmp"
        return 0
    fi
    rm -f "$wri_tmp"
    return 1
}

rc_has_path_dir() { # rc_has_path_dir <rc_file> <dir>
    [ -f "$1" ] && [ -n "$2" ] || return 1
    SPID3R_AWK_DIR="$2" awk '
        BEGIN {
            dir = ENVIRON["SPID3R_AWK_DIR"]
            dlen = length(dir)
            if (dlen == 0) exit 1
            sq = sprintf("%c", 39)
            found = 0
        }
        {
            line = $0
            code = ""
            in_sq = 0
            in_dq = 0
            n = length(line)
            for (i = 1; i <= n; i++) {
                c = substr(line, i, 1)
                if (c == "\\" && in_dq && i < n) {
                    code = code substr(line, i, 2)
                    i++
                    continue
                }
                if (c == sq && !in_dq) {
                    in_sq = !in_sq
                } else if (c == "\"" && !in_sq) {
                    in_dq = !in_dq
                } else if (c == "#" && !in_sq && !in_dq) {
                    if (i == 1 || substr(line, i - 1, 1) ~ /[ \t;|&(]/) {
                        break
                    }
                }
                code = code c
            }
            if (code !~ /(^|[ \t;|&(])(PATH|path)[ \t]*[+]?=/) next
            s = code
            while ((idx = index(s, dir)) > 0) {
                pre = (idx == 1) ? "" : substr(s, idx - 1, 1)
                post = substr(s, idx + dlen, 1)
                post2 = substr(s, idx + dlen + 1, 1)
                pre_ok = (pre == "" || pre == ":" || pre == "\"" || pre == sq || pre == "=" || pre == " " || pre == "\t" || pre == "(")
                post_ok = (post == "" || post == ":" || post == "\"" || post == sq || post == " " || post == "\t" || post == ";" || post == ")" || (post == "/" && (post2 == "" || post2 == ":" || post2 == "\"" || post2 == sq || post2 == " " || post2 == "\t" || post2 == ";" || post2 == ")")))
                if (pre_ok && post_ok) { found = 1; exit 0 }
                s = substr(s, idx + 1)
            }
        }
        END { exit(found ? 0 : 1) }
    ' "$1" 2>/dev/null
}

configure_path() {
    if [ "$NO_MODIFY_PATH" -eq 1 ]; then
        return 0
    fi

    cp_need_inst=1
    cp_need_tool=1
    case ":${PATH:-}:" in
        *":${INSTALL_DIR}:"*|*":${INSTALL_DIR}/:"*) cp_need_inst=0 ;;
    esac
    case ":${PATH:-}:" in
        *":${TOOL_BIN_DIR}:"*|*":${TOOL_BIN_DIR}/:"*) cp_need_tool=0 ;;
    esac
    if [ "$INSTALL_DIR" = "$TOOL_BIN_DIR" ]; then
        cp_need_tool=0
    fi
    if [ "$cp_need_inst" -eq 0 ] && [ "$cp_need_tool" -eq 0 ]; then
        path_already=1
        return 0
    fi

    cp_rc_exists=0
    cp_any_need_inst=0
    cp_any_need_tool=0
    for rc in "${HOME_PREFIX}/.bashrc" "${HOME_PREFIX}/.zshrc" "${HOME_PREFIX}/.profile"; do
        [ -f "$rc" ] || continue
        cp_rc_exists=1
        if [ "$cp_need_inst" -eq 1 ] && ! rc_has_path_dir "$rc" "$INSTALL_DIR"; then
            cp_any_need_inst=1
        fi
        if [ "$cp_need_tool" -eq 1 ] && ! rc_has_path_dir "$rc" "$TOOL_BIN_DIR"; then
            cp_any_need_tool=1
        fi
    done
    if [ "$cp_rc_exists" -eq 1 ] && [ "$cp_any_need_inst" -eq 0 ] && [ "$cp_any_need_tool" -eq 0 ]; then
        path_in_rc=1
        return 0
    fi
    if [ "$cp_rc_exists" -eq 0 ]; then
        cp_any_need_inst="$cp_need_inst"
        cp_any_need_tool="$cp_need_tool"
    fi

    if [ "$cp_any_need_inst" -eq 1 ] && [ "$cp_any_need_tool" -eq 1 ]; then
        cp_prompt_dirs="${INSTALL_DIR} and ${TOOL_BIN_DIR}"
    elif [ "$cp_any_need_inst" -eq 1 ]; then
        cp_prompt_dirs="${INSTALL_DIR}"
    else
        cp_prompt_dirs="${TOOL_BIN_DIR}"
    fi

    if [ "$MODIFY_PATH_OPT" -eq 0 ] && [ -t 0 ] && [ -t 1 ]; then
        printf 'Add %s to PATH in your shell profile (~/.bashrc, ~/.zshrc, ~/.profile)? [Y/n] ' "$cp_prompt_dirs"
        IFS= read -r cp_reply || cp_reply="y"
        case "$cp_reply" in
            n|N|no|NO|No)
                say "Skipped PATH setup."
                return 0
                ;;
        esac
    fi

    cp_comment='# Spid3r (installed by install.sh)'
    cp_exp_inst="export PATH=\"${INSTALL_DIR}:\$PATH\""
    cp_exp_tool="export PATH=\"${TOOL_BIN_DIR}:\$PATH\""
    cp_exp_both="export PATH=\"${INSTALL_DIR}:${TOOL_BIN_DIR}:\$PATH\""

    if [ ! -f "${HOME_PREFIX}/.bashrc" ] && [ ! -f "${HOME_PREFIX}/.zshrc" ] && [ ! -f "${HOME_PREFIX}/.profile" ]; then
        touch "${HOME_PREFIX}/.profile"
    fi

    for rc in "${HOME_PREFIX}/.bashrc" "${HOME_PREFIX}/.zshrc" "${HOME_PREFIX}/.profile"; do
        [ -f "$rc" ] || continue
        cp_add_inst=0
        cp_add_tool=0
        if [ "$cp_need_inst" -eq 1 ] && ! rc_has_path_dir "$rc" "$INSTALL_DIR"; then
            cp_add_inst=1
        fi
        if [ "$cp_need_tool" -eq 1 ] && ! rc_has_path_dir "$rc" "$TOOL_BIN_DIR"; then
            cp_add_tool=1
        fi
        if [ "$cp_add_inst" -eq 0 ] && [ "$cp_add_tool" -eq 0 ]; then
            continue
        fi

        # Upgrade an existing install.sh single-directory export line in place
        # so re-installing over an older version adds TOOL_BIN_DIR cleanly
        # without leaving two separate installer blocks.
        if [ "$cp_add_inst" -eq 0 ] && [ "$cp_add_tool" -eq 1 ] && grep -qFx -- "$cp_exp_inst" "$rc" 2>/dev/null; then
            cp_tmp="${rc}.spid3r_path.$$"
            if SPID3R_AWK_OLD="$cp_exp_inst" SPID3R_AWK_NEW="$cp_exp_both" awk '
                BEGIN {
                    old_line = ENVIRON["SPID3R_AWK_OLD"]
                    new_line = ENVIRON["SPID3R_AWK_NEW"]
                }
                $0 == old_line { print new_line; next }
                { print }
            ' "$rc" > "$cp_tmp" && write_rc_inplace "$cp_tmp" "$rc"; then
                say "PATH updated: added ${TOOL_BIN_DIR} to ${rc}"
                path_added=1
                continue
            else
                rm -f "$cp_tmp"
            fi
        fi

        if [ "$cp_add_inst" -eq 1 ] && [ "$cp_add_tool" -eq 1 ]; then
            cp_export="$cp_exp_both"
            cp_added_desc="${INSTALL_DIR} and ${TOOL_BIN_DIR}"
        elif [ "$cp_add_inst" -eq 1 ]; then
            cp_export="$cp_exp_inst"
            cp_added_desc="${INSTALL_DIR}"
        else
            cp_export="$cp_exp_tool"
            cp_added_desc="${TOOL_BIN_DIR}"
        fi

        {
            printf '\n%s\n' "$cp_comment"
            printf '%s\n' "$cp_export"
        } >> "$rc"
        say "PATH updated: added ${cp_added_desc} to ${rc}"
        path_added=1
    done
}

remove_path_from_rc() { # remove_path_from_rc <rc_file>
    rpf_rc="$1"
    [ -f "$rpf_rc" ] || return 0
    rpf_comment='# Spid3r (installed by install.sh)'
    rpf_exp_both="export PATH=\"${INSTALL_DIR}:${TOOL_BIN_DIR}:\$PATH\""
    rpf_exp_inst="export PATH=\"${INSTALL_DIR}:\$PATH\""
    rpf_exp_tool="export PATH=\"${TOOL_BIN_DIR}:\$PATH\""
    if ! grep -qFx -- "$rpf_comment" "$rpf_rc" 2>/dev/null \
        && ! grep -qFx -- "$rpf_exp_both" "$rpf_rc" 2>/dev/null \
        && ! grep -qFx -- "$rpf_exp_inst" "$rpf_rc" 2>/dev/null \
        && ! grep -qFx -- "$rpf_exp_tool" "$rpf_rc" 2>/dev/null; then
        return 0
    fi

    rpf_tmp="${rpf_rc}.spid3r_uninst.$$"
    if SPID3R_AWK_CMT="$rpf_comment" \
       SPID3R_AWK_BOTH="$rpf_exp_both" \
       SPID3R_AWK_INST="$rpf_exp_inst" \
       SPID3R_AWK_TOOL="$rpf_exp_tool" \
       awk '
        BEGIN {
            cmt = ENVIRON["SPID3R_AWK_CMT"]
            exp_both = ENVIRON["SPID3R_AWK_BOTH"]
            exp_inst = ENVIRON["SPID3R_AWK_INST"]
            exp_tool = ENVIRON["SPID3R_AWK_TOOL"]
            skip_next = 0
            removed = 0
        }
        {
            if (skip_next) {
                skip_next = 0
                if ($0 == exp_both || $0 == exp_inst || $0 == exp_tool || $0 ~ /^export PATH=".*:\$PATH"$/) {
                    removed = 1
                    next
                }
                print cmt
            }
            if ($0 == cmt) {
                skip_next = 1
                next
            }
            if ($0 == exp_both || $0 == exp_inst || $0 == exp_tool) {
                removed = 1
                next
            }
            print
        }
        END {
            if (skip_next) {
                print cmt
            }
            exit(removed ? 0 : 2)
        }
    ' "$rpf_rc" > "$rpf_tmp"; then
        if write_rc_inplace "$rpf_tmp" "$rpf_rc"; then
            say "Removed Spid3r PATH entry from ${rpf_rc}"
        fi
    else
        rm -f "$rpf_tmp"
    fi
}

do_uninstall() {
    say "Uninstalling Spid3r from ${INSTALL_DIR}..."
    for bin_name in spid3r sp yamc yamcvm spvm spkg spid3r-self spid3r-control; do
        target_p="${INSTALL_DIR}/${bin_name}"
        if [ -e "$target_p" ] || [ -L "$target_p" ]; then
            rm -f "$target_p"
        fi
    done
    rm -f "${INSTALL_DIR}"/.*.new.* 2>/dev/null || true

    remove_legacy_stdlib

    parent_dir="$(dirname "$INSTALL_DIR")"
    share_dir="${parent_dir}/share/spid3r"
    if [ -e "${share_dir}/spid3r-formatter.vsix" ]; then
        rm -f "${share_dir}/spid3r-formatter.vsix"
    fi
    rmdir "$share_dir" 2>/dev/null || true
    rmdir "${parent_dir}/share" 2>/dev/null || true

    lib_dir="$(lib_dir_for_install)"
    case "$lib_dir" in
        "${INSTALL_DIR}/lib"|*/.spid3r/lib|*/spid3r/lib)
            if [ -d "$lib_dir" ]; then
                for lf in "$lib_dir"/*.so "$lib_dir"/*.so.* "$lib_dir"/*.dylib; do
                    [ -e "$lf" ] || [ -L "$lf" ] || continue
                    rm -f "$lf"
                done
                rmdir "$lib_dir" 2>/dev/null || true
            fi
            ;;
    esac

    for rc in "${HOME_PREFIX}/.bashrc" "${HOME_PREFIX}/.zshrc" "${HOME_PREFIX}/.profile"; do
        remove_path_from_rc "$rc"
    done

    rmdir "$INSTALL_DIR" 2>/dev/null || true
    case "$parent_dir" in
        */.spid3r|*/spid3r)
            rmdir "$parent_dir" 2>/dev/null || true
            ;;
    esac

    say "Spid3r uninstall complete."
}

print_post_install_hint() {
    say ""
    if [ "$path_added" -eq 1 ]; then
        say "Open a new terminal (or run: source ~/.bashrc) and then try:"
    elif [ "$path_already" -eq 1 ]; then
        say "PATH already configured. Try:"
    elif [ "$path_in_rc" -eq 1 ]; then
        say "PATH already configured in your shell profile. Open a new terminal (or run: source ~/.bashrc) and then try:"
    elif [ "$TOOL_BIN_DIR" != "$INSTALL_DIR" ]; then
        say "PATH was not modified. Add ${INSTALL_DIR} and ${TOOL_BIN_DIR} to PATH or run ${INSTALL_DIR}/spid3r:"
    else
        say "PATH was not modified. Add ${INSTALL_DIR} to PATH or run ${INSTALL_DIR}/spid3r:"
    fi
    say "    spid3r --help              # or: sp --help"
    say "    spid3r run hello.matrix    # or: sp run hello.mt  /  spvm hello.mt"
    say "    yamc hello.mt -o hello     # native AOT build (YamLLVM)"
    say "    yamcvm hello.mt            # compile .matrixc bytecode"
    say "    spkg init myapp            # package & project commands"
    say "    spid3r control status      # runtime dashboard (config, safety, logs)"

    if [ -n "${GITHUB_PATH-}" ]; then
        say ""
        say "GitHub Actions detected: ${INSTALL_DIR} is not on PATH for the current step."
        say "To expose Spid3r and spkg tools to subsequent steps of this job, run before the next step:"
        say "    echo \"${INSTALL_DIR}\" >> \"\$GITHUB_PATH\""
        if [ "$TOOL_BIN_DIR" != "$INSTALL_DIR" ]; then
            say "    echo \"${TOOL_BIN_DIR}\" >> \"\$GITHUB_PATH\""
        fi
    fi
}

if [ "$DO_UNINSTALL" -eq 1 ]; then
    do_uninstall
    exit 0
fi

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
        for c in llvm-config llvm-config-23 /usr/lib/llvm-23/bin/llvm-config; do
            if command -v "$c" >/dev/null 2>&1; then have_llvm=1; break; fi
        done
        if command -v pkg-config >/dev/null 2>&1 \
            && pkg-config --exists webkit2gtk-4.1 2>/dev/null; then have_webkit=1; fi
        [ "$have_llvm" -eq 1 ]   && FEATURES="native"
        [ "$have_webkit" -eq 1 ] && FEATURES="${FEATURES:+$FEATURES,}gui"
        [ "$have_llvm" -eq 1 ]   || warn "LLVM 23 not found: skipping the native backend (sudo apt install llvm-23-dev)"
        [ "$have_webkit" -eq 1 ] || warn "WebKit2GTK 4.1 not found: skipping 'spid3r gui' (sudo apt install libwebkit2gtk-4.1-dev)"
    fi

    say "Building spid3r from source (this compiles the whole toolchain)..."
    PARENT_DIR="$(dirname "$INSTALL_DIR")"
    mkdir -p "$INSTALL_DIR"

    if [ -n "$FEATURES" ]; then
        say "Features: ${FEATURES}"
        case ",${FEATURES}," in
            *,gui,*|*" gui "*|*" gui,"*|*",gui "*)
                cargo install --locked --git "https://github.com/${REPO}.git" spid3r \
                    --bin spid3r --bin spid3r-control \
                    --root "$PARENT_DIR" --no-default-features --features "$FEATURES"
                ;;
            *)
                cargo install --locked --git "https://github.com/${REPO}.git" spid3r \
                    --bin spid3r \
                    --root "$PARENT_DIR" --no-default-features --features "$FEATURES"
                ;;
        esac
    else
        say "Features: core only (VM + interpreter, zero dependencies)"
        cargo install --locked --git "https://github.com/${REPO}.git" spid3r \
            --bin spid3r \
            --root "$PARENT_DIR" --no-default-features
    fi

    if [ -f "${PARENT_DIR}/bin/spid3r" ] && [ "${INSTALL_DIR}" != "${PARENT_DIR}/bin" ]; then
        atomic_install_file "${PARENT_DIR}/bin/spid3r" "${INSTALL_DIR}/spid3r" 0755
        if [ -f "${PARENT_DIR}/bin/spid3r-control" ]; then
            atomic_install_file "${PARENT_DIR}/bin/spid3r-control" "${INSTALL_DIR}/spid3r-control" 0755
        fi
    fi
    if [ -x "${INSTALL_DIR}/spid3r" ]; then
        for alias_bin in sp yamc yamcvm spvm spkg; do
            ln -sf spid3r "${INSTALL_DIR}/${alias_bin}"
        done
    fi

    remove_legacy_stdlib
    configure_path

    "${INSTALL_DIR}/spid3r" control ini --init >/dev/null 2>&1 \
        || warn "could not write the default spid3r.ini (run: spid3r control ini --init)"

    install_editor_extension ""

    say "Done. spid3r is installed into ${INSTALL_DIR}."
    print_post_install_hint
    exit 0
fi

# ------------------------------------------------------------------- checks --
command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 \
    || die "curl or wget is required to download the installer payload"

command -v tar >/dev/null 2>&1 \
    || die "tar is required to extract the installer payload"

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
SELF_SRC="$(find "$TMPDIR_DL" -type f -name spid3r-self | head -n 1 || true)"

# ----------------------------------------------------------------- install ---
mkdir -p "$INSTALL_DIR"
atomic_install_file "$BIN_SRC" "${INSTALL_DIR}/spid3r" 0755 \
    || die "failed to install spid3r into ${INSTALL_DIR}"
for alias_bin in sp yamc yamcvm spvm spkg; do
    ln -sf spid3r "${INSTALL_DIR}/${alias_bin}"
done
if [ -n "$SELF_SRC" ] && [ -f "$SELF_SRC" ]; then
    atomic_install_file "$SELF_SRC" "${INSTALL_DIR}/spid3r-self" 0755 \
        || die "failed to install spid3r-self into ${INSTALL_DIR}"
fi
# Install any bundled shared libraries (e.g. libLLVM.so.23.1 and its bundled
# transitive deps on Linux) atomically into the $ORIGIN-relative lib directory.
LIB_DIR="$(lib_dir_for_install)"
BUNDLED_LIBS="$(find "$TMPDIR_DL" \( -type f -o -type l \) \( -name '*.so' -o -name '*.so.*' -o -name '*.dylib' \) 2>/dev/null || true)"
if [ -n "$BUNDLED_LIBS" ]; then
    mkdir -p "$LIB_DIR"
    printf '%s\n' "$BUNDLED_LIBS" | while IFS= read -r lib_f; do
        [ -e "$lib_f" ] || continue
        atomic_install_file "$lib_f" "${LIB_DIR}/$(basename "$lib_f")" 0755 \
            || die "failed to install bundled library $(basename "$lib_f")"
    done
fi

# ---------------------------------------------------------- stdlib cleanup ---
remove_legacy_stdlib

# --------------------------------------------------------------- PATH set ----
configure_path

# -------------------------------------------------------------- verify -------
if [ -x "${INSTALL_DIR}/spid3r" ]; then
    VERSION_OUT="$("${INSTALL_DIR}/spid3r" --version 2>&1)" \
        || die "installed binary failed to run (${INSTALL_DIR}/spid3r --version): ${VERSION_OUT}"
    if [ -x "${INSTALL_DIR}/spid3r-self" ]; then
        SELF_OUT="$("${INSTALL_DIR}/spid3r-self" --version 2>&1)" \
            || die "installed selfhost binary failed to run (${INSTALL_DIR}/spid3r-self --version): ${SELF_OUT}"
    fi
    say ""
    say "Spid3r installed successfully:"
    printf '%s\n' "$VERSION_OUT"
    "${INSTALL_DIR}/spid3r" control ini --init >/dev/null 2>&1 \
        || warn "could not write the default spid3r.ini (run: spid3r control ini --init)"
else
    die "installation finished but the binary is not executable at ${INSTALL_DIR}/spid3r"
fi

install_editor_extension "$TMPDIR_DL"
print_post_install_hint
