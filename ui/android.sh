#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# The Tauri CLI for Android (`./android.sh build --apk`, `./android.sh dev`), with what the vendored
# OpenSSL under SQLCipher needs from the NDK: NDK r23 and later have no `<target>-ranlib`, which
# OpenSSL's build looks for otherwise. Needs NDK_HOME (and ANDROID_HOME, JAVA_HOME for Gradle).
set -euo pipefail
: "${NDK_HOME:?set NDK_HOME to the Android NDK}"
ranlib=$(echo "$NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-ranlib)
[ -x "$ranlib" ] || { echo "no llvm-ranlib in $NDK_HOME" >&2; exit 1; }
for target in aarch64_linux_android armv7_linux_androideabi i686_linux_android x86_64_linux_android; do
  export "RANLIB_$target=$ranlib"
done
cd "$(dirname "$0")"
exec npm run tauri -- android "$@"
