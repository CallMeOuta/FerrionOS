#!/usr/bin/env bash
set -euo pipefail

HOST_ARCH=$(uname -m)
if [[ $(uname -s) != Linux ]]; then
    echo "Rust helper installation requires a Linux host." >&2
    exit 1
fi

RUST_TOOLCHAIN=1.100.0
RUSTUP_VERSION=1.28.2
SCCACHE_VERSION=0.15.0
JUST_VERSION=1.50.0
CBINDGEN_VERSION=0.29.0

case "$HOST_ARCH" in
    x86_64)
        RUSTUP_SHA256=20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c
        SCCACHE_SHA256=782d2b5dd7ae0a55ebe368ab258114d0928d019ac2d949ab85d5d02f3926709e
        JUST_SHA256=27e011cd6328fadd632e59233d2cf5f18460b8a8c4269acd324c1a8669f34db0
        CBINDGEN_NAME=ubuntu22.04
        CBINDGEN_SHA256=2cc8a248e12632a0249f17d7370ad4a38c98292bdf7bb1357f737f8b6e8cc435
        ;;
    aarch64)
        RUSTUP_SHA256=e3853c5a252fca15252d07cb23a1bdd9377a8c6f3efa01531109281ae47f841c
        SCCACHE_SHA256=3a6a3712b49da3d263bf2d30d702de4302793016019e800bfb81c0c69401d8f8
        JUST_SHA256=3beb4967ce05883cf09ac12d6d128166eb4c6d0b03eff74b61018a6880655d7d
        CBINDGEN_NAME=ubuntu22.04-aarch64
        CBINDGEN_SHA256=a9f4b5322f9ea7b8637984e385ec1c73c8e4e42faf2818da85f87ab77a356ddc
        ;;
    *)
        echo "Unsupported Linux host architecture: $HOST_ARCH (expected x86_64 or aarch64)." >&2
        exit 1
        ;;
esac

DOWNLOAD_DIR=$(mktemp -d)
trap 'rm -rf -- "$DOWNLOAD_DIR"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

download_verified() {
    local url=$1 sha256=$2 destination=$3
    curl --fail --location --silent --show-error --retry 3 --retry-delay 1 "$url" -o "$destination.partial"
    printf '%s  %s\n' "$sha256" "$destination.partial" | sha256sum --check -
    mv -- "$destination.partial" "$destination"
}

echo "Installing rust..."
download_verified \
    "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${HOST_ARCH}-unknown-linux-gnu/rustup-init" \
    "$RUSTUP_SHA256" "$DOWNLOAD_DIR/rustup-init"
chmod +x "$DOWNLOAD_DIR/rustup-init"
"$DOWNLOAD_DIR/rustup-init" -y --default-toolchain "$RUST_TOOLCHAIN" --profile minimal

mkdir -p ~/.cargo/bin

echo "Downloading sccache..."
download_verified \
    "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-${HOST_ARCH}-unknown-linux-musl.tar.gz" \
    "$SCCACHE_SHA256" "$DOWNLOAD_DIR/sccache.tar.gz"
tar -xzf "$DOWNLOAD_DIR/sccache.tar.gz" -C ~/.cargo/bin --strip-components=1 \
    "sccache-v${SCCACHE_VERSION}-${HOST_ARCH}-unknown-linux-musl/sccache"

echo "Downloading just..."
download_verified \
    "https://github.com/casey/just/releases/download/${JUST_VERSION}/just-${JUST_VERSION}-${HOST_ARCH}-unknown-linux-musl.tar.gz" \
    "$JUST_SHA256" "$DOWNLOAD_DIR/just.tar.gz"
tar -xzf "$DOWNLOAD_DIR/just.tar.gz" -C ~/.cargo/bin just

echo "Downloading cbindgen..."
download_verified \
    "https://github.com/mozilla/cbindgen/releases/download/${CBINDGEN_VERSION}/cbindgen-${CBINDGEN_NAME}" \
    "$CBINDGEN_SHA256" "$DOWNLOAD_DIR/cbindgen"
install -m 0755 "$DOWNLOAD_DIR/cbindgen" ~/.cargo/bin/cbindgen
