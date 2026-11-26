#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Writes the secrets compose.yaml reads, once. Keep ./secrets backed up: without master_key the
# notes on the server can't be read.
set -euo pipefail
cd "$(dirname "$0")"
umask 077
mkdir -p secrets
if [ -e secrets/master_key ]; then
  echo "secrets/ already exists; not overwriting it" >&2
  exit 1
fi
random() { openssl rand -hex 32; }
random > secrets/postgres_password
db=$(random); printf '%s' "$db" > secrets/db_password
printf 'postgres://drusite:%s@postgres:5432/drusite' "$db" > secrets/database_url
s3=$(random); printf '%s' "$s3" > secrets/s3_secret_key
cat > secrets/s3.json <<JSON
{"identities": [{"name": "drusite",
  "credentials": [{"accessKey": "drusite", "secretKey": "$s3"}],
  "actions": ["Admin", "Read", "Write", "List", "Tagging"]}]}
JSON
docker run --rm "ghcr.io/pranavajk1/drusite-server:${DRUSITE_VERSION:-main}" keys generate > secrets/master_key
# The containers' users must be able to read them.
chmod 644 secrets/*
echo "wrote secrets/; next: edit drusite.toml, then docker compose up -d"
