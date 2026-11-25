#!/bin/sh
set -eu
cd "$SRCROOT/../.."
export PATH="$PATH:$HOME/.cargo/bin"
export CARGO_TARGET_DIR="$ZXS_RUST_BUILD_DIR"
export IPHONEOS_DEPLOYMENT_TARGET=17.0
# Rust build scripts link against macOS, while the target selects its iOS SDK.
SDKROOT=$(xcrun --sdk macosx --show-sdk-path)
export SDKROOT
cargo build --offline --locked --release --target "$ZXS_RUST_TARGET" -p zxs-ffi
