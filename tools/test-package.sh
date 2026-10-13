#!/bin/sh
# Installs, upgrades, removes, and purges a jackerymon .deb in a throwaway
# container, and checks the user and the conffile along the way.
#
#   tools/test-package.sh DEB [IMAGE [PLATFORM]]
#
# IMAGE defaults to debian:bookworm, the oldest target; PLATFORM is a
# docker platform such as linux/arm64 (needs qemu binfmt).
set -eu
deb=$(realpath "$1")
image=${2:-debian:bookworm}
platform=${3:-}
name=$(basename "$deb")
docker run --rm ${platform:+--platform "$platform"} \
  -v "$(dirname "$deb"):/deb:ro" "$image" sh -eu -c '
apt-get update -qq >/dev/null
apt_get() { DEBIAN_FRONTEND=noninteractive apt-get -y -qq "$@" >/dev/null 2>&1; }
apt_get install "/deb/$1"
id jackerymon >/dev/null
test -d /etc/jackerymon -a -d /var/lib/jackerymon -o ! -d /run/systemd/system
echo "install ok"
apt_get install --reinstall "/deb/$1"
echo "upgrade ok"
apt_get remove jackerymon
test -e /etc/default/jackerymon
echo "remove ok, conffile kept"
apt_get purge jackerymon
test ! -e /etc/default/jackerymon
echo "purge ok, conffile gone"
' test-package "$name"
