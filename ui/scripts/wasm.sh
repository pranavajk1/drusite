#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Builds the core as WebAssembly for the browser version: drusite-wasm with the `wasm` profile,
# then wasm-bindgen's JavaScript glue, into src/core/web/wasm/ (not checked in).
#
# Needs wasm-bindgen-cli at the version Cargo.lock has (`cargo install wasm-bindgen-cli
# --version <v> --locked`), and an `llvm-ar` that knows wasm objects: SQLCipher is C, and the
# system `ar` on macOS writes archives wasm's linker can't index. The toolchain's own
# (`rustup component add llvm-tools`) is used when there's none on the PATH.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$here/.." && pwd)"
out="$here/src/core/web/wasm"

if [[ -z "${AR_wasm32_unknown_unknown:-}" ]]; then
  if command -v llvm-ar >/dev/null; then
    export AR_wasm32_unknown_unknown=llvm-ar
  else
    sysroot="$(rustc --print sysroot)"
    found="$(find "$sysroot/lib/rustlib" -name llvm-ar -type f 2>/dev/null | head -1)"
    if [[ -z "$found" ]]; then
      echo "no llvm-ar: install LLVM, or run: rustup component add llvm-tools" >&2
      exit 1
    fi
    export AR_wasm32_unknown_unknown="$found"
  fi
fi

want="$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' "$root/Cargo.lock")"
have="$(wasm-bindgen --version 2>/dev/null | awk '{ print $2 }' || true)"
if [[ "$have" != "$want" ]]; then
  echo "wasm-bindgen $want is needed (found: ${have:-none}): cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi

cd "$root"
cargo build --locked --package drusite-wasm --target wasm32-unknown-unknown --profile wasm
rm -rf "$out"
wasm-bindgen --target web --remove-name-section --remove-producers-section --out-dir "$out" "target/wasm32-unknown-unknown/wasm/drusite_wasm.wasm"
echo "wrote $out"
