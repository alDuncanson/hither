#!/bin/sh
# Run hither, installing it first if this machine does not have it yet.
#
#   curl -fsSL https://alduncanson.github.io/hither/run.sh | sh -s -- <ticket>
#   curl -fsSL https://alduncanson.github.io/hither/run.sh | sh -s -- to <inbox> photos/
#
# Everything after `--` is passed to hither unchanged. Files land in the
# folder you run this from. Installing puts one binary in ~/.local/bin
# (override with HITHER_INSTALL_DIR); nothing else is touched.
set -eu

main() {
  install_dir="${HITHER_INSTALL_DIR:-$HOME/.local/bin}"
  if command -v hither >/dev/null 2>&1; then
    bin=$(command -v hither)
  elif [ -x "${install_dir}/hither" ]; then
    bin="${install_dir}/hither"
  else
    printf '%s\n' "hither is not installed yet; installing it first." >&2
    curl -fsSL https://alduncanson.github.io/hither/install.sh | HITHER_INSTALL_DIR="${install_dir}" sh
    bin="${install_dir}/hither"
    printf '\n' >&2
  fi
  if [ $# -eq 0 ]; then
    exec "${bin}" --help
  fi
  exec "${bin}" "$@"
}

# Defined above, called here: the whole script is parsed before anything
# runs, so a cut-off download does nothing instead of half of something.
main "$@"
