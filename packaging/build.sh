#!/usr/bin/env bash
# Produce a self-contained Ubuntu 22.04 tarball from the current source tree.
#
#   packaging/build.sh          → dist/ragpilot-<version>-x86_64-linux-gnu.tar.gz
#   packaging/build.sh --verify → also unpack and install it in a clean 22.04
#
# The build runs inside ubuntu:22.04 so the binary links against glibc 2.35.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT=$PWD
VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$VERSION" ] || { echo "cannot read version from Cargo.toml" >&2; exit 1; }

NAME="ragpilot-$VERSION-x86_64-linux-gnu"
STAGE="dist/$NAME"
VERIFY=false
[ "${1:-}" = --verify ] && VERIFY=true

echo "==> building $NAME in ubuntu:22.04"
rm -rf "$STAGE" && mkdir -p "$STAGE"
IMAGE=ragpilot-build:$VERSION
docker build --file packaging/Dockerfile.ubuntu2204 --tag "$IMAGE" .

# Lift the binary out of the image. `docker cp` needs a container, not a
# running one — create makes the filesystem addressable without starting it.
cid=$(docker create "$IMAGE")
trap 'docker rm -f "$cid" >/dev/null 2>&1 || true' EXIT
docker cp "$cid:/out/ragpilot" "$STAGE/ragpilot"

cp packaging/install.sh "$STAGE/install.sh"
cp README.md LICENSE "$STAGE/"
chmod 755 "$STAGE/ragpilot" "$STAGE/install.sh"

tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
echo
echo "==> dist/$NAME.tar.gz"
( cd dist && sha256sum "$NAME.tar.gz" | tee "$NAME.tar.gz.sha256" )
ls -lh "dist/$NAME.tar.gz" | awk '{print "    " $5}'

# ── verification ───────────────────────────────────────────────────────────
# Building in 22.04 is not proof that it runs in 22.04: the build image has a
# compiler and its dependencies, a user's machine has neither. Install the
# tarball into a stock ubuntu:22.04 and make the binary talk.
if $VERIFY; then
    echo
    echo "==> installing into a stock ubuntu:22.04"
    docker run --rm -i \
        -v "$ROOT/dist/$NAME.tar.gz:/pkg.tar.gz:ro" \
        ubuntu:22.04 bash -euo pipefail -c '
            tar -xzf /pkg.tar.gz -C /tmp
            cd /tmp/'"$NAME"'
            ./install.sh
            echo
            echo "--- ldd ---"
            ldd /usr/local/bin/ragpilot
            echo "--- it runs ---"
            ragpilot --version
            ragpilot --help | head -3
        '
fi
