#!/bin/sh
# Install hither: https://github.com/alDuncanson/hither
#
#   curl -fsSL https://alduncanson.github.io/hither/install.sh | sh
#
# Downloads the latest release for this machine, checks its SHA-256, and puts
# the `hither` binary in ~/.local/bin (override with HITHER_INSTALL_DIR).
# Pin a version with HITHER_VERSION=v0.1.0. Nothing else is touched.
set -eu

REPO="alDuncanson/hither"
INSTALL_DIR="${HITHER_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*" >&2; }
fail() { say "hither install: $*"; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "this script needs '$1'"; }

need curl; need tar; need uname; need mktemp

os=$(uname -s); arch=$(uname -m)
case "$os" in
  Darwin) os_t="apple-darwin" ;;
  Linux)  os_t="unknown-linux-gnu" ;;
  MINGW*|MSYS*|CYGWIN*) fail "Windows is not packaged yet. Use WSL, or build from source with cargo." ;;
  *) fail "unsupported operating system: $os" ;;
esac
case "$arch" in
  arm64|aarch64) arch_t="aarch64" ;;
  x86_64|amd64)  arch_t="x86_64" ;;
  *) fail "unsupported architecture: $arch" ;;
esac
target="$arch_t-$os_t"

if [ -n "${HITHER_VERSION:-}" ]; then
  tag="$HITHER_VERSION"
else
  # Newest release, prereleases included; /releases/latest would skip alphas.
  tag=$(curl -fsSL -H "Accept: application/vnd.github+json" \
        "https://api.github.com/repos/$REPO/releases?per_page=1" \
        | tr -d '\n' | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p')
  [ -n "$tag" ] || fail "could not find a release. Is GitHub reachable?"
fi

asset="hither-${tag}-${target}.tar.gz"
base="https://github.com/${REPO}/releases/download/${tag}"
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t hither)
trap 'rm -rf "$tmp"' EXIT

say "Fetching hither ${tag} for ${target}..."
curl -fsSL -o "${tmp}/${asset}" "${base}/${asset}" || fail "no build for ${target} in ${tag}"
curl -fsSL -o "${tmp}/${asset}.sha256" "${base}/${asset}.sha256" || fail "checksum missing for ${asset}"

(
  cd "$tmp"
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 -c "${asset}.sha256" >/dev/null
  elif command -v sha256sum >/dev/null 2>&1; then sha256sum -c "${asset}.sha256" >/dev/null
  else say "warning: no sha256 tool found, skipping checksum"
  fi
) || fail "checksum did not match; refusing to install"

tar -xzf "${tmp}/${asset}" -C "${tmp}"
mkdir -p "$INSTALL_DIR"
install -m 755 "${tmp}/hither" "${INSTALL_DIR}/hither"
say "Installed $("${INSTALL_DIR}/hither" --version) to ${INSTALL_DIR}/hither"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    say ""
    say "${INSTALL_DIR} is not on your PATH. Add this to your shell profile:"
    say "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    say "or run it directly: ${INSTALL_DIR}/hither"
    ;;
esac
