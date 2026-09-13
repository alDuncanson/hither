#!/bin/sh
# Run hither, installing or updating it first if needed.
#
#   curl -fsSL https://alduncanson.github.io/hither/run.sh | sh -s -- <ticket>
#   curl -fsSL https://alduncanson.github.io/hither/run.sh | sh -s -- to <inbox> photos/
#
# Everything after `--` is passed to hither unchanged. Files land in your
# Downloads folder. This is the path for people who just want the files, so
# it keeps hither current: if the installed version differs from the newest
# release it reinstalls first (skip that with HITHER_NO_UPDATE=1). If GitHub
# cannot be reached, the installed version is used as is. Installing puts
# one binary in ~/.local/bin (override with HITHER_INSTALL_DIR).
set -eu

REPO="alDuncanson/hither"
SITE="https://alduncanson.github.io/hither"

say() { printf '%s\n' "$*" >&2; }

latest_version() {
  curl -fsSL --max-time 5 -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/${REPO}/releases?per_page=1" 2>/dev/null \
    | tr -d '\n' | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p'
}

installed_version() {
  "$1" --version 2>/dev/null | awk '{print $2}'
}

install_into() {
  curl -fsSL "${SITE}/install.sh" | HITHER_INSTALL_DIR="$1" sh
}

main() {
  install_dir="${HITHER_INSTALL_DIR:-$HOME/.local/bin}"
  bin=""
  if command -v hither >/dev/null 2>&1; then
    bin=$(command -v hither)
  elif [ -x "${install_dir}/hither" ]; then
    bin="${install_dir}/hither"
  fi

  if [ -z "${bin}" ]; then
    say "hither is not installed yet; installing it first."
    install_into "${install_dir}"
    bin="${install_dir}/hither"
    say ""
  elif [ -z "${HITHER_NO_UPDATE:-}" ]; then
    have=$(installed_version "${bin}" || true)
    latest=$(latest_version || true)
    if [ -n "${have}" ] && [ -n "${latest}" ] && [ "${have}" != "${latest}" ]; then
      say "Updating hither ${have} -> ${latest}"
      if install_into "$(dirname "${bin}")"; then
        say ""
      else
        say "Update did not finish; using hither ${have}."
      fi
    fi
  fi

  if [ $# -eq 0 ]; then
    exec "${bin}" --help
  fi
  exec "${bin}" "$@"
}

# Defined above, called here: the whole script is parsed before anything
# runs, so a cut-off download does nothing instead of half of something.
main "$@"
