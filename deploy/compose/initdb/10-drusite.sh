#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Runs once, on an empty data directory.
set -eu
password=$(cat /run/secrets/db_password)
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" -v password="$password" <<'SQL'
CREATE ROLE drusite LOGIN PASSWORD :'password';
CREATE DATABASE drusite OWNER drusite;
\connect drusite
CREATE EXTENSION vector;
SQL
