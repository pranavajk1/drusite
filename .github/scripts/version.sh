#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Prints Drusite's version, after checking that every place holding it agrees: the workspace's
# Cargo.toml (which the server and the crates report) and the app's tauri.conf.json (which the
# Android and iOS builds use). With a git ref like refs/tags/v1.2.3, the tag must match too.
#
#   .github/scripts/version.sh [ref]
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml)
tauri=$(jq -r .version crates/drusite-app/tauri.conf.json)

if [[ -z "$cargo" ]]; then
  echo "no version in Cargo.toml's [workspace.package]" >&2
  exit 1
fi
if [[ "$cargo" != "$tauri" ]]; then
  echo "Cargo.toml says $cargo, but crates/drusite-app/tauri.conf.json says $tauri" >&2
  exit 1
fi
ref=${1:-}
if [[ "$ref" == refs/tags/v* && "${ref#refs/tags/v}" != "$cargo" ]]; then
  echo "the tag is ${ref#refs/tags/}, but the version is $cargo" >&2
  exit 1
fi
echo "$cargo"
