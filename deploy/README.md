# Running a Drusite server

One binary, `drusite-server`, with three things of yours: PostgreSQL 16 or newer with pgvector,
an S3-compatible store, and somewhere to keep a master key. Nothing assumes a particular proxy,
store, identity provider or secret manager. Every setting is in
[`crates/drusite-server/drusite.example.toml`](../crates/drusite-server/drusite.example.toml);
each can also come from a `DRUSITE__SECTION__KEY` variable, and any value from a file
(`url_file = ...`).

## The image

`ghcr.io/pranavajk1/drusite-server`, for amd64 and arm64: distroless, no shell, running as a
non-root user. `main` follows the main branch; version tags (`1.2`, `1.2.3`) follow releases.
Each image carries an SBOM and build provenance, and is signed with cosign by the workflow that
built it:

```bash
cosign verify ghcr.io/pranavajk1/drusite-server:main \
  --certificate-identity-regexp '^https://github.com/pranavajk1/drusite/\.github/workflows/image\.yml@' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

## Docker Compose

[`compose/`](compose/) runs the server with PostgreSQL and SeaweedFS on one machine:

```bash
cd deploy/compose
./setup.sh                 # writes the secrets, once; back up secrets/
$EDITOR drusite.toml       # public_url and the login methods
docker compose up -d
docker compose run --rm drusite account create you@example.com
```

## Kubernetes

[`helm/drusite`](helm/drusite) is a chart for the server alone; bring the database and the store.
Its `config` value is the config file, and `secret.name` names a Secret whose keys are mounted
under `/run/secrets/drusite/` for the config's `*_file` settings to read:

```bash
kubectl create secret generic drusite \
  --from-literal=database-url='postgres://drusite:…@postgres:5432/drusite' \
  --from-literal=s3-secret-access-key='…' \
  --from-literal=master-key="$(docker run --rm ghcr.io/pranavajk1/drusite-server:main keys generate)"
helm install drusite deploy/helm/drusite --set secret.name=drusite -f my-values.yaml
```

Whatever sits in front of the server must pass WebSockets (`/api/v1/sync`). Auth lives in
Drusite, so don't put forward-auth or other proxy login in front of it: the apps can't follow
it.
