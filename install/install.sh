#!/bin/sh
# hush installer for macOS and Linux.
#
#   curl -fsSL https://github.com/oele-isis-vanderbilt/hush-gst/releases/latest/download/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --no-ffmpeg
#
# Options:
#   --version <v>        install this release tag (default: latest)   [HUSH_VERSION]
#   --dir <path>         install root (default: ~/.hush)               [HUSH_INSTALL_DIR]
#   --no-ffmpeg          do not download the FFmpeg sidecar
#   --ffmpeg-from-path   skip the sidecar if ffmpeg + ffprobe are already on PATH
#   --no-modify-path     do not edit shell startup files
#   --uninstall          remove hush and the PATH line
#
# HUSH_DOWNLOAD_BASE overrides where archives come from (a URL or file:// dir),
# for testing.

set -eu

REPO="oele-isis-vanderbilt/hush-gst"
MARKER="# added by the hush installer"

version="${HUSH_VERSION:-latest}"
root="${HUSH_INSTALL_DIR:-$HOME/.hush}"
want_ffmpeg=1
ffmpeg_from_path=0
modify_path=1
uninstall=0

say() { printf '%s\n' "hush-install: $*"; }
err() { printf '%s\n' "hush-install: error: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || err "--version needs a value"; version="$2"; shift 2 ;;
        --dir) [ $# -ge 2 ] || err "--dir needs a value"; root="$2"; shift 2 ;;
        --no-ffmpeg) want_ffmpeg=0; shift ;;
        --ffmpeg-from-path) ffmpeg_from_path=1; shift ;;
        --no-modify-path) modify_path=0; shift ;;
        --uninstall) uninstall=1; shift ;;
        -h|--help) sed -n '2,19p' "$0" 2>/dev/null || true; exit 0 ;;
        *) err "unknown option: $1" ;;
    esac
done

bin_dir="$root/bin"

# Shell startup files that get the PATH line.
rc_files() {
    case "$(basename "${SHELL:-sh}")" in
        zsh) echo "${ZDOTDIR:-$HOME}/.zshrc" ;;
        bash)
            echo "$HOME/.bashrc"
            [ "$(uname -s)" = Darwin ] && echo "$HOME/.bash_profile"
            ;;
        fish) echo "$HOME/.config/fish/config.fish" ;;
        *) echo "$HOME/.profile" ;;
    esac
}

remove_path_lines() {
    for rc in "$HOME/.zshrc" "${ZDOTDIR:-$HOME}/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile" \
              "$HOME/.profile" "$HOME/.config/fish/config.fish"; do
        [ -f "$rc" ] || continue
        if grep -qF "$MARKER" "$rc"; then
            tmp="$rc.hush-tmp"
            grep -vF "$MARKER" "$rc" > "$tmp" || true
            cat "$tmp" > "$rc" && rm -f "$tmp"
            say "removed PATH line from $rc"
        fi
    done
}

if [ "$uninstall" -eq 1 ]; then
    if [ -d "$root" ]; then
        # Only delete a directory this installer created.
        [ -f "$root/.hush-install" ] || err "$root was not created by this installer; not removing it"
        rm -rf "$root"
        say "removed $root"
    else
        say "$root does not exist"
    fi
    remove_path_lines
    exit 0
fi

# --- platform -----------------------------------------------------------------

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux) os_part="unknown-linux-gnu" ;;
    *) err "unsupported OS: $os (on Windows, use install.ps1)" ;;
esac
case "$arch" in
    x86_64|amd64) arch_part="x86_64" ;;
    arm64|aarch64) arch_part="aarch64" ;;
    *) err "unsupported CPU: $arch" ;;
esac
# Rosetta: prefer the native arm64 build.
if [ "$os" = Darwin ] && [ "$arch_part" = x86_64 ] && \
   [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
    arch_part="aarch64"
fi
target="$arch_part-$os_part"

# --- download helpers ---------------------------------------------------------

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q -O "$2" "$1"; }
else
    err "need curl or wget"
fi

if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    err "need sha256sum or shasum"
fi

if [ -n "${HUSH_DOWNLOAD_BASE:-}" ]; then
    base="$HUSH_DOWNLOAD_BASE"
elif [ "$version" = latest ]; then
    base="https://github.com/$REPO/releases/latest/download"
else
    base="https://github.com/$REPO/releases/download/$version"
fi

# --- install ------------------------------------------------------------------

archive="hush-$target.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $archive ($version)"
fetch "$base/$archive" "$tmp/$archive" || err "download failed: $base/$archive"
fetch "$base/$archive.sha256" "$tmp/$archive.sha256" || err "download failed: $base/$archive.sha256"

expected="$(cut -d' ' -f1 < "$tmp/$archive.sha256")"
actual="$(sha256 "$tmp/$archive")"
[ "$expected" = "$actual" ] || err "checksum mismatch for $archive (expected $expected, got $actual)"

mkdir -p "$tmp/x"
tar -xzf "$tmp/$archive" -C "$tmp/x"
src="$(find "$tmp/x" -type f -name hush | head -n 1)"
[ -n "$src" ] || err "archive does not contain hush"

mkdir -p "$bin_dir"
cp "$src" "$bin_dir/hush.new"
chmod 755 "$bin_dir/hush.new"
mv -f "$bin_dir/hush.new" "$bin_dir/hush"
if [ "$os" = Darwin ]; then
    xattr -d com.apple.quarantine "$bin_dir/hush" 2>/dev/null || true
fi
: > "$root/.hush-install"
say "installed $bin_dir/hush"

# --- ffmpeg sidecar -----------------------------------------------------------

if [ "$want_ffmpeg" -eq 1 ]; then
    if [ "$ffmpeg_from_path" -eq 1 ] && command -v ffmpeg >/dev/null 2>&1 && command -v ffprobe >/dev/null 2>&1; then
        say "using ffmpeg from PATH ($(command -v ffmpeg))"
    elif ! "$bin_dir/hush" ffmpeg install --dir "$root/ffmpeg"; then
        say "warning: FFmpeg sidecar not installed. Install ffmpeg yourself (brew / apt / dnf),"
        say "         or run 'hush ffmpeg install' later."
    fi
fi

# --- PATH -----------------------------------------------------------------------

case ":$PATH:" in
    *":$bin_dir:"*) on_path=1 ;;
    *) on_path=0 ;;
esac
if [ "$modify_path" -eq 1 ] && [ "$on_path" -eq 0 ]; then
    for rc in $(rc_files); do
        if [ -f "$rc" ] && grep -qF "$MARKER" "$rc"; then
            continue
        fi
        mkdir -p "$(dirname "$rc")"
        case "$rc" in
            *config.fish) printf 'fish_add_path "%s" %s\n' "$bin_dir" "$MARKER" >> "$rc" ;;
            *) printf "export PATH=\"%s:\$PATH\" %s\n" "$bin_dir" "$MARKER" >> "$rc" ;;
        esac
        say "added $bin_dir to PATH in $rc"
    done
fi

echo
"$bin_dir/hush" doctor || true
echo
if [ "$on_path" -eq 0 ]; then
    say "done. Restart your shell (or run: export PATH=\"$bin_dir:\$PATH\"), then: hush --help"
else
    say "done. Run: hush --help"
fi
