#!/bin/sh
# Installs the `frood` program of a release: on Linux from the release's core image, on macOS
# the release's file from GitHub (Frood itself then runs in Docker Desktop).
#
#   sh install.sh 0.1.8            # into /usr/local/bin (sudo when needed)
#   FROOD_BIN=~/bin/frood sh install.sh 0.1.8
#
# Needs Docker with Compose v2. On Linux the program runs on the Linux it was built for
# (Debian 13) and newer.
set -eu

VERSION=${1:?which release, e.g. 0.1.8}
IMAGE=${FROOD_IMAGE:-ghcr.io/froodai/frood}:$VERSION
RELEASES=${FROOD_RELEASES:-https://github.com/froodai/frood/releases/download}
DEST=${FROOD_BIN:-/usr/local/bin/frood}

command -v docker >/dev/null || { echo "Docker is needed: https://docs.docker.com/engine/install/" >&2; exit 1; }
docker compose version >/dev/null 2>&1 || { echo "Docker Compose v2 is needed" >&2; exit 1; }

tmp=$(mktemp)
case "$(uname -s)" in
  Darwin)
    trap 'rm -f "$tmp" "$tmp.tar.gz"' EXIT
    curl -fsSL "$RELEASES/v$VERSION/frood-macos.tar.gz" -o "$tmp.tar.gz" \
      || { echo "release $VERSION has no macOS program (it comes with 0.1.9 and later)" >&2; exit 1; }
    tar -xzf "$tmp.tar.gz" -O frood > "$tmp"
    ;;
  *)
    docker pull --quiet "$IMAGE" >/dev/null
    container=$(docker create "$IMAGE")
    trap 'docker rm -f "$container" >/dev/null 2>&1; rm -f "$tmp"' EXIT
    docker cp "$container:/usr/local/bin/frood" "$tmp" >/dev/null
    ;;
esac
chmod 0755 "$tmp"
if [ -w "$(dirname "$DEST")" ]; then
  mv "$tmp" "$DEST"
else
  sudo install -m 0755 "$tmp" "$DEST"
fi
"$DEST" --version
echo "next: frood init --dir <folder> — it asks a few questions and can start Frood"
