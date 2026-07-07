#!/bin/sh
# CursorDump installer: downloads the latest prebuilt binary from GitHub
# Releases, verifies its sha256, and installs it. No Rust toolchain required.
#
#   curl -fsSL https://raw.githubusercontent.com/lpalbou/cursordump/main/install.sh | sh
#
# Environment overrides:
#   CURSORDUMP_VERSION   tag to install (default: latest release)
#   CURSORDUMP_BIN_DIR   install directory (default: ~/.local/bin)

set -eu

REPO="lpalbou/cursordump"
BIN_DIR="${CURSORDUMP_BIN_DIR:-$HOME/.local/bin}"

err() { printf 'error: %s\n' "$1" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || err "curl is required"
command -v tar >/dev/null 2>&1 || err "tar is required"

# --- detect platform ---------------------------------------------------------
os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin)
    # An x86_64 shell under Rosetta still wants the native arm64 build.
    if [ "$arch" = "x86_64" ] && [ "$(sysctl -in sysctl.proc_translated 2>/dev/null)" = "1" ]; then
      arch="arm64"
    fi
    case "$arch" in
      arm64)  target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *) err "unsupported macOS architecture: $arch" ;;
    esac ;;
  Linux)
    # MUSL-static builds: run on any distro, glibc or musl.
    case "$arch" in
      x86_64)          target="x86_64-unknown-linux-musl" ;;
      aarch64 | arm64) target="aarch64-unknown-linux-musl" ;;
      *) err "unsupported Linux architecture: $arch" ;;
    esac ;;
  *)
    err "unsupported OS: $os (build from source: cargo install --git https://github.com/$REPO)" ;;
esac

# --- resolve version ---------------------------------------------------------
if [ -n "${CURSORDUMP_VERSION:-}" ]; then
  version="$CURSORDUMP_VERSION"
else
  version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1) || true
  [ -n "${version:-}" ] || err "could not determine the latest release — either no release is published yet, or the GitHub API is unreachable/rate-limited. Pin one with CURSORDUMP_VERSION=vX.Y.Z"
fi

name="cursordump-$version-$target"
url="https://github.com/$REPO/releases/download/$version/$name.tar.gz"

# --- download, verify, install -----------------------------------------------
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

printf 'downloading %s ...\n' "$url"
curl -fsSL "$url" -o "$tmp/$name.tar.gz" || err "download failed (no release asset for $target?)"

# Checksum verification is mandatory: every release publishes the .sha256.
curl -fsSL "$url.sha256" -o "$tmp/$name.tar.gz.sha256" || err "checksum download failed — retry, or verify manually from the releases page"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmp" && sha256sum -c "$name.tar.gz.sha256" >/dev/null) || err "checksum mismatch"
elif command -v shasum >/dev/null 2>&1; then
  (cd "$tmp" && shasum -a 256 -c "$name.tar.gz.sha256" >/dev/null) || err "checksum mismatch"
else
  err "no sha256 tool found (need sha256sum or shasum)"
fi

tar xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$BIN_DIR"
install -m 755 "$tmp/$name/cursordump" "$BIN_DIR/cursordump"

printf 'installed cursordump %s to %s\n' "$version" "$BIN_DIR/cursordump"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) printf 'note: %s is not on your PATH — add:  export PATH="%s:$PATH"\n' "$BIN_DIR" "$BIN_DIR" ;;
esac
printf 'run: cursordump\n'
