#!/bin/sh
# Installs the `frood` program of a release: it is in the release's core image.
#
#   sh install.sh 0.1.0            # into /usr/local/bin (sudo when needed)
#   FROOD_BIN=~/bin/frood sh install.sh 0.1.0
#
# Needs Docker with Compose v2. The program runs on the Linux it was built for (Debian 13) and
# newer; elsewhere run it from the image.
set -eu

VERSION=${1:?which release, e.g. 0.1.0}
IMAGE=${FROOD_IMAGE:-ghcr.io/froodai/frood}:$VERSION
DEST=${FROOD_BIN:-/usr/local/bin/frood}

command -v docker >/dev/null || { echo "Docker is needed: https://docs.docker.com/engine/install/" >&2; exit 1; }
docker compose version >/dev/null 2>&1 || { echo "Docker Compose v2 is needed" >&2; exit 1; }

docker pull --quiet "$IMAGE" >/dev/null
container=$(docker create "$IMAGE")
tmp=$(mktemp)
trap 'docker rm -f "$container" >/dev/null 2>&1; rm -f "$tmp"' EXIT
docker cp "$container:/usr/local/bin/frood" "$tmp" >/dev/null
chmod 0755 "$tmp"
if [ -w "$(dirname "$DEST")" ]; then
  mv "$tmp" "$DEST"
else
  sudo install -m 0755 "$tmp" "$DEST"
fi
"$DEST" --version
echo "next: frood init --dir <folder>, edit frood.yaml, frood apply"
