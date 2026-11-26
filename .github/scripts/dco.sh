#!/usr/bin/env bash
# Checks that every non-merge commit in a range carries a DCO sign-off matching its author.
# usage: dco.sh <base> <head>    (base may be empty or all zeros: check everything up to head)
set -euo pipefail

base="${1:-}"
head="${2:?usage: dco.sh <base> <head>}"
if [[ -z "$base" || "$base" =~ ^0+$ ]]; then
  range="$head"
else
  range="$base..$head"
fi

failed=0
checked=0
while read -r sha; do
  [[ -z "$sha" ]] && continue
  checked=$((checked + 1))
  author="$(git log -1 --format='%an <%ae>' "$sha")"
  if ! git log -1 --format='%(trailers:key=Signed-off-by,valueonly)' "$sha" | grep -qxF "$author"; then
    echo "::error::commit $(git log -1 --format='%h %s' "$sha") has no 'Signed-off-by: $author'"
    failed=1
  fi
done < <(git rev-list --no-merges "$range")

if [[ "$failed" -ne 0 ]]; then
  echo "Sign off each commit with 'git commit -s' (see CONTRIBUTING.md), e.g. 'git rebase --signoff $base'."
  exit 1
fi
echo "DCO: $checked commit(s) signed off"
