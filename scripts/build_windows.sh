#!/bin/sh
# Build the Windows executable from Linux or WSL. The linker is in .cargo/config.toml (MinGW-w64, for
# example the package mingw-w64 of Ubuntu). Add the target once: rustup target add x86_64-pc-windows-gnu
set -e
cd "$(git rev-parse --show-toplevel)"
cargo build --release --locked -p eoview --target x86_64-pc-windows-gnu
echo "$PWD/target/x86_64-pc-windows-gnu/release/eoview.exe"
