#!/bin/sh
# Builds the wasm core (crates/atrium-orca-core) and copies it next to the plugin.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
cd "$repo"
cargo build --profile wasm-release --target wasm32-wasip1 -p atrium-orca-core
mkdir -p "$here/../core"
cp "target/wasm32-wasip1/wasm-release/atrium_orca_core.wasm" "$here/../core/atrium_core.wasm"
ls -l "$here/../core/atrium_core.wasm"
