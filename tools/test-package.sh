#!/bin/bash
#
# Installs, upgrades, removes, and purges a jackerymon .deb in a throwaway
# container, and checks the user and the conffile along the way.
#
#   tools/test-package.sh DEB [IMAGE [PLATFORM]]
#
# IMAGE defaults to debian:bookworm, the oldest target; PLATFORM is a
# docker platform such as linux/arm64 (needs qemu binfmt).

set -euo pipefail

readonly DEFAULT_IMAGE="debian:bookworm"

# Runs in the container, with the package's file name as $1.
# shellcheck disable=SC2016  # Expanded by the container's shell, not ours.
readonly CONTAINER_SCRIPT='
set -euo pipefail
apt_get() {
  DEBIAN_FRONTEND=noninteractive apt-get -y -qq "$@" >/dev/null 2>&1
}
apt-get update -qq >/dev/null
apt_get install "/deb/${1}"
id jackerymon >/dev/null
[[ -d /etc/jackerymon && -d /var/lib/jackerymon || ! -d /run/systemd/system ]]
echo "install ok"
apt_get install --reinstall "/deb/${1}"
echo "upgrade ok"
apt_get remove jackerymon
[[ -e /etc/default/jackerymon ]]
echo "remove ok, conffile kept"
apt_get purge jackerymon
[[ ! -e /etc/default/jackerymon ]]
echo "purge ok, conffile gone"
'

main() {
  if (($# < 1 || $# > 3)); then
    echo "usage: ${0} DEB [IMAGE [PLATFORM]]" >&2
    exit 2
  fi
  local deb
  deb=$(realpath "${1}")
  local -r image=${2:-${DEFAULT_IMAGE}}
  local -r platform=${3:-}
  local platform_args=()
  if [[ -n "${platform}" ]]; then
    platform_args=(--platform "${platform}")
  fi
  docker run --rm "${platform_args[@]}" \
    -v "$(dirname "${deb}"):/deb:ro" "${image}" \
    bash -c "${CONTAINER_SCRIPT}" test-package "$(basename "${deb}")"
}

main "$@"
