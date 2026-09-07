#!/bin/sh
# RagPilot installer — Ubuntu 22.04 and newer, x86_64.
#
#   ./install.sh                  install (system-wide if permitted, else user)
#   ./install.sh --prefix ~/.local  install somewhere specific
#   ./install.sh --uninstall      remove it again
set -eu

BIN=ragpilot
SRC_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PREFIX=
ACTION=install

while [ $# -gt 0 ]; do
    case $1 in
        --prefix) PREFIX=${2:?--prefix needs a directory}; shift 2 ;;
        --prefix=*) PREFIX=${1#--prefix=}; shift ;;
        --uninstall) ACTION=uninstall; shift ;;
        -h|--help) sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1  (try --help)" >&2; exit 2 ;;
    esac
done

die() { echo "error: $*" >&2; exit 1; }

# Where to put it. Root, or a writable /usr/local/bin, means system-wide;
# otherwise the user's own bin, which needs no sudo.
if [ -z "$PREFIX" ]; then
    if [ "$(id -u)" = 0 ] || [ -w /usr/local/bin ]; then
        PREFIX=/usr/local
    else
        PREFIX=$HOME/.local
    fi
fi
DEST=$PREFIX/bin

if [ "$ACTION" = uninstall ]; then
    if [ -e "$DEST/$BIN" ]; then
        rm -f "$DEST/$BIN" || die "cannot remove $DEST/$BIN — try with sudo"
        echo "removed $DEST/$BIN"
    else
        echo "nothing to remove at $DEST/$BIN"
    fi
    echo
    echo "Your indexes and the brain are untouched, under"
    echo "  ${RAGPILOT_DATA_DIR:-$HOME/.local/share/ragpilot}"
    echo "Delete that directory too if you want a clean slate."
    exit 0
fi

# ── preflight ──────────────────────────────────────────────────────────────
[ -f "$SRC_DIR/$BIN" ] || die "$BIN not found next to this script"

arch=$(uname -m)
[ "$arch" = x86_64 ] || die "this build is x86_64 only; this machine is $arch.
Build from source instead:  cargo install ragpilot"

# The binary is linked against glibc 2.35. Say so now, in a sentence, rather
# than letting the loader say it in six.
if command -v ldd >/dev/null 2>&1; then
    have=$(ldd --version 2>/dev/null | head -1 | tr ' ' '\n' | tail -1)
    case $have in
        2.*) major=${have#2.}; major=${major%%.*}
             [ "${major:-0}" -ge 35 ] 2>/dev/null || die "needs glibc 2.35 or newer (Ubuntu 22.04+); found $have" ;;
    esac
fi

mkdir -p "$DEST" || die "cannot create $DEST"
[ -w "$DEST" ] || die "$DEST is not writable — re-run with sudo, or:
  ./install.sh --prefix \$HOME/.local"

install -m 755 "$SRC_DIR/$BIN" "$DEST/$BIN" || die "install into $DEST failed"

version=$("$DEST/$BIN" --version 2>&1) || die "installed, but $DEST/$BIN would not run:
$version"
echo "installed $version → $DEST/$BIN"

# ── what is left for the user to do ────────────────────────────────────────
case ":$PATH:" in
    *":$DEST:"*) ;;
    *) echo
       echo "warning: $DEST is not on your PATH. Add it:"
       echo "  echo 'export PATH=\"$DEST:\$PATH\"' >> ~/.bashrc && . ~/.bashrc" ;;
esac

# The first index downloads the embedding model over HTTPS. A machine without
# a CA bundle fails there, several minutes in, with a TLS error that says
# nothing about the real cause.
if [ ! -e /etc/ssl/certs/ca-certificates.crt ] && [ ! -e /etc/pki/tls/certs/ca-bundle.crt ]; then
    echo
    echo "warning: no CA certificate bundle found. The first index downloads the"
    echo "embedding model over HTTPS and will fail without one:"
    echo "  sudo apt-get install -y ca-certificates"
fi

# Only claim Qdrant is down if we were actually able to ask. Without curl the
# check proves nothing, so say the neutral thing instead of the wrong one.
qdrant_hint="  docker run -d --name qdrant -p 6333:6333 -p 6334:6334 qdrant/qdrant"
if ! command -v curl >/dev/null 2>&1; then
    echo
    echo "RagPilot stores its index in Qdrant. If you do not have one running:"
    echo "$qdrant_hint"
elif ! curl -fsS --max-time 2 http://localhost:6333/readyz >/dev/null 2>&1; then
    echo
    echo "RagPilot stores its index in Qdrant, which is not answering on"
    echo "localhost:6333 yet. Start one:"
    echo "$qdrant_hint"
fi

echo
echo "Then, in a project:"
echo "  ragpilot init . claude     # index it and register the MCP server"
echo "  ragpilot doctor            # check the installation"
