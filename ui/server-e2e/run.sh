#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Runs the identity end-to-end test: starts PostgreSQL (pgvector), Mailpit and Keycloak in
# Docker, builds and starts drusite-server with every login method on, drives it with
# identity.mjs in Chromium, and cleans up. Needs Docker, Rust, and `npm ci` done in ui/.
# HEADED=1 shows the browser.
set -euo pipefail
cd "$(dirname "$0")/../.."

PG=pgvector/pgvector:0.8.7-pg16@sha256:484e838076e04939d27d3e176e91c86eea96236126023be8d3059761122e579f
MAILPIT=axllent/mailpit:v1.34.0
KEYCLOAK=quay.io/keycloak/keycloak:26.8.2
S3=versity/versitygw:v1.11.0@sha256:8319a530d08d537426e2f855bae5280709795dc73a3f0c347973ef915d6343c5
work=$(mktemp -d)
server_pid=

cleanup() {
  [ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null || true
  docker rm -f drusite-e2e-pg drusite-e2e-mailpit drusite-e2e-keycloak drusite-e2e-s3 >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

docker run -d --name drusite-e2e-pg -p 55499:5432 -e POSTGRES_PASSWORD=postgres "$PG" >/dev/null
docker run -d --name drusite-e2e-mailpit -p 58025:8025 -p 51025:1025 "$MAILPIT" >/dev/null
# Identity doesn't store blobs, but the server checks its store is writable at start-up.
mkdir -p "$work/s3/drusite"
docker run -d --name drusite-e2e-s3 -p 57099:7070 -v "$work/s3:/data" "$S3" \
  --access testkey --secret testsecret posix /data >/dev/null
docker run -d --name drusite-e2e-keycloak -p 58180:8080 \
  -e KC_BOOTSTRAP_ADMIN_USERNAME=admin -e KC_BOOTSTRAP_ADMIN_PASSWORD=admin "$KEYCLOAK" start-dev >/dev/null

cargo build --quiet --package drusite-server

until docker exec drusite-e2e-pg pg_isready -U postgres >/dev/null 2>&1; do sleep 1; done
sleep 2
docker exec drusite-e2e-pg psql -qU postgres \
  -c "CREATE ROLE drusite LOGIN PASSWORD 'drusite'" -c "CREATE DATABASE drusite OWNER drusite" >/dev/null
docker exec drusite-e2e-pg psql -qU postgres -d drusite -c "CREATE EXTENSION vector" >/dev/null

until curl -sf -o /dev/null http://localhost:58180/realms/master; do sleep 2; done
bash ui/server-e2e/keycloak.sh >/dev/null

DRUSITE_MASTER_KEY=$(./target/debug/drusite-server keys generate)
export DRUSITE_MASTER_KEY
./target/debug/drusite-server -c ui/server-e2e/drusite.toml serve >"$work/server.log" 2>&1 &
server_pid=$!
until curl -sf -o /dev/null http://localhost:18080/readyz; do
  kill -0 "$server_pid" 2>/dev/null || { cat "$work/server.log"; exit 1; }
  sleep 1
done

status=0
(cd ui && node server-e2e/identity.mjs) || status=$?
if grep -E '"level":"(WARN|ERROR)"' "$work/server.log"; then
  echo "the server logged warnings or errors" >&2
  status=1
fi
exit $status
