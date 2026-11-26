#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# A Keycloak realm for the identity end-to-end test: client "drusite", a "groups" claim, the
# group "notes", and three users (alice: verified, in notes; bob: unverified, in notes; carol:
# verified, not in notes). Password for all: kc-test-pass.
set -euo pipefail
K() { docker exec drusite-e2e-keycloak /opt/keycloak/bin/kcadm.sh "$@"; }
K config credentials --server http://localhost:8080 --realm master --user admin --password admin >/dev/null
K create realms -s realm=drusite -s enabled=true >/dev/null
K create clients -r drusite -s clientId=drusite -s secret=kc-test-secret \
  -s 'redirectUris=["http://localhost:18080/api/v1/auth/oidc/kc/callback"]' -s publicClient=false \
  -s standardFlowEnabled=true -s directAccessGrantsEnabled=false \
  -s 'attributes={"pkce.code.challenge.method":"S256"}' >/dev/null
CID=$(K get clients -r drusite -q clientId=drusite --fields id --format csv --noquotes)
K create clients/$CID/protocol-mappers/models -r drusite -s name=groups -s protocol=openid-connect \
  -s protocolMapper=oidc-group-membership-mapper \
  -s 'config={"claim.name":"groups","full.path":"false","id.token.claim":"true","access.token.claim":"true","userinfo.token.claim":"true"}' >/dev/null
K create groups -r drusite -s name=notes >/dev/null
GID=$(K get groups -r drusite -q search=notes --fields id --format csv --noquotes)
for u in "alice alice@drusite.test true notes" "bob bob@drusite.test false notes" "carol carol@drusite.test true none"; do
  set -- $u
  K create users -r drusite -s username=$1 -s email=$2 -s emailVerified=$3 -s enabled=true -s firstName=$1 -s lastName=Test >/dev/null
  K set-password -r drusite --username $1 --new-password kc-test-pass
  if [ "$4" = notes ]; then
    UID_=$(K get users -r drusite -q username=$1 -q exact=true --fields id --format csv --noquotes)
    K update users/$UID_/groups/$GID -r drusite -s realm=drusite -s userId=$UID_ -s groupId=$GID -n
  fi
done
echo done
