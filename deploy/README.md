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

## Search

Server search is on by default: Postgres full-text search over every block, kept up to date by
a background indexer a couple of seconds after each change. Add an embedding model to search by
meaning too. By default it's an OpenAI-compatible endpoint (LiteLLM, Ollama, vLLM, SGLang, LM
Studio, llama.cpp, …), which needs its address:

```sh
DRUSITE__SEARCH__EMBEDDINGS__BASE_URL=http://litellm:4000/v1
DRUSITE__SEARCH__EMBEDDINGS__MODEL=text-embedding-3-small
DRUSITE__SEARCH__EMBEDDINGS__API_KEY_FILE=/run/secrets/embeddings
```

Or a vendor, through its own API: `PROVIDER` is `openai`, `google`, `mistral` or `openrouter`
(Anthropic makes no embeddings). A vendor needs a key and no address; `BASE_URL` replaces its
address if set (a proxy, a regional endpoint).

```sh
DRUSITE__SEARCH__EMBEDDINGS__PROVIDER=google
DRUSITE__SEARCH__EMBEDDINGS__MODEL=gemini-embedding-001
DRUSITE__SEARCH__EMBEDDINGS__API_KEY_FILE=/run/secrets/gemini
DRUSITE__SEARCH__EMBEDDINGS__DIMENSIONS=768
```

The apps' AI features (chat, and embeddings on the device) are set up in each app's Settings →
AI, with keys kept on that device; the server's config covers only what the server does.

The database holds each block's words (a `tsvector`) and its embedding, never its text: snippets
are made by opening the sealed document when someone searches. Words and embeddings are still
derived from the text and reveal what it's about to anyone who can read the database or its
backups. `DRUSITE__SEARCH__ENABLED=false` keeps them out (and turns server search off).

Every vector is stored with its model's ID. Changing the model embeds everything again in the
background; searches use only vectors from the current model.

## Attachments

Files on pages (images, PDFs, anything) are stored in the object store you configured under
`[storage]`, each one sealed with its workspace's key, beside the documents. Devices upload a file
when they sync and download one when it's opened. The largest file a page can hold is 100 MiB
unless set:

```sh
DRUSITE__ATTACHMENTS__MAX_BYTES=262144000
```

A page names each file by the BLAKE3 hash of its content. The server never stores that hash: it
finds a file by the hash keyed with the workspace's key, so neither the database nor the bucket
alone can confirm that a workspace holds a file someone already has.

A reverse proxy in front of Drusite must accept request bodies that large too (for example
Traefik's `buffering` middleware, or `client_max_body_size` in nginx).

## Agents (MCP)

Agents such as Claude Code connect at `https://<your server>/mcp` (MCP over Streamable HTTP).
To connect Claude Code, run:

```sh
claude mcp add --transport http drusite https://notes.example.com/mcp
```

The agent signs in through the browser like an app does, using a
[Client ID Metadata Document](https://datatracker.ietf.org/doc/draft-ietf-oauth-client-id-metadata-document/)
(no registration step). Then a consent page asks what it may do:

- which workspaces it can see;
- read only, or read and edit;
- whether it may edit journal entries (it can't unless you allow it).

Its tokens are good at `/mcp` only (RFC 8707). Agents have no tools that delete. Every change one
makes is recorded, and the app lists those changes with an undo button for each.

An agent that can't open a browser, such as one running on a server, uses a token from the
account page (`/account` → Agents → Make a token), sent as `Authorization: Bearer <token>`.
Disconnecting an agent on that page ends its access at once.

| Setting | Default | |
|---|---|---|
| `DRUSITE__AGENTS__ENABLED` | `true` | `false` turns `/mcp` off |
| `DRUSITE__AGENTS__MAX_TOKEN_DAYS` | `365` | the longest an account-page token can last |
| `DRUSITE__AGENTS__ALLOW_INSECURE_CLIENT_IDS` | `false` | development only: lets client IDs be plain-HTTP or private-network URLs |

When the server fetches a client's metadata document, it refuses addresses on private
networks. Your reverse proxy must pass the `/.well-known/oauth-protected-resource` paths through
to Drusite.
