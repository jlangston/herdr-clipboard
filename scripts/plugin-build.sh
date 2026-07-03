#!/usr/bin/env sh
# herdr [[build]] entrypoint: fetch the prebuilt release binary matching this
# checkout's manifest version, verify its checksum, and place it where the
# manifest's pane/event commands expect it. Falls back to building from
# source whenever the prebuilt path isn't viable, so a Rust toolchain is
# only needed when there is no usable release asset.
set -eu

REPO="jlangston/herdr-clipboard"
DEST="target/release/herdr-clip"
VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' herdr-plugin.toml | head -n1)"

fallback() {
    echo "herdr-clip: $1" >&2
    if command -v cargo >/dev/null 2>&1; then
        echo "herdr-clip: building from source instead" >&2
        exec cargo build --release
    fi
    echo "herdr-clip: no prebuilt binary usable and no Rust toolchain for a source build" >&2
    exit 1
}

[ -n "$VERSION" ] || fallback "could not read version from herdr-plugin.toml"
command -v curl >/dev/null 2>&1 || fallback "curl not found"
command -v tar >/dev/null 2>&1 || fallback "tar not found"

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
    Linux-aarch64) TARGET=aarch64-unknown-linux-gnu ;;
    Darwin-arm64) TARGET=aarch64-apple-darwin ;;
    Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
    *) fallback "no prebuilt binary for $(uname -s)/$(uname -m)" ;;
esac

ASSET="herdr-clip-$TARGET.tar.gz"
BASE="https://github.com/$REPO/releases/download/v$VERSION"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSL --retry 2 -o "$TMP/$ASSET" "$BASE/$ASSET" || fallback "download failed: $BASE/$ASSET"
curl -fsSL --retry 2 -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" || fallback "checksum download failed"

# sha256sum on Linux, shasum on macOS; both accept the SHA256SUMS format.
if command -v sha256sum >/dev/null 2>&1; then
    SUMCMD="sha256sum"
else
    SUMCMD="shasum -a 256"
fi
(cd "$TMP" && grep " $ASSET\$" SHA256SUMS | $SUMCMD -c - >/dev/null) \
    || fallback "checksum mismatch for $ASSET"

mkdir -p "$(dirname "$DEST")"
tar -xzf "$TMP/$ASSET" -C "$(dirname "$DEST")"
chmod +x "$DEST"
echo "herdr-clip: installed prebuilt $TARGET binary for v$VERSION" >&2
