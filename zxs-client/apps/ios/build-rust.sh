#!/bin/sh
set -eu
cd "$SRCROOT/../.."
export PATH="$PATH:$HOME/.cargo/bin"
export CARGO_TARGET_DIR="$DERIVED_FILE_DIR/zxs-rust"
export IPHONEOS_DEPLOYMENT_TARGET=17.0
cargo build --offline --locked --release --target "$ZXS_RUST_TARGET" -p zxs-ffi
