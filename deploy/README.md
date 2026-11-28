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

Every vector is stored with its model's ID (with `@<dimensions>` added when `DIMENSIONS` is set,
since a shorter vector is another space). Changing the model, or its dimensions, embeds everything
again in the background: when the server starts, the old model's vectors and vector indexes are
dropped, and the indexer re-embeds every page. Until it's done, search by meaning finds less and
full-text search carries on. A sweep every ten minutes picks up any page left without vectors (say,
after the embedding model was down for a while). Any size of vector works; past 2,000 dimensions
pgvector can't build its HNSW index, so searches scan instead, which is fine for a personal
notebook.

## Ask your notes

People can ask their notes a question in the apps and get an answer written by a chat model from
their own pages, with a link to each block it used. Answers are made on the server, the same way for
every app: the server finds the blocks (full-text search, plus semantic search when
`[search.embeddings]` is set), only in a workspace the person can read, sends them with the
question to the chat model, and streams the answer back. Conversations are saved in the account, so
every device has them.

It needs a chat model, configured apart from the embedding model. Without one, the apps say Ask
isn't set up and point here. Any provider works:

```sh
# A self-hosted model behind an OpenAI-compatible endpoint: LiteLLM, Ollama, vLLM, SGLang,
# llama.cpp's server, LM Studio, …
DRUSITE__ASK__CHAT__BASE_URL=http://ollama:11434/v1
DRUSITE__ASK__CHAT__MODEL=qwen3:4b
# Optional: a key if the endpoint wants one, and the name the apps show.
DRUSITE__ASK__CHAT__API_KEY_FILE=/run/secrets/chat
DRUSITE__ASK__CHAT__NAME="Qwen 3 (home server)"
```

Or a vendor through its own API: `PROVIDER` is `anthropic`, `openai`, `google`, `mistral` or
`openrouter`. A vendor needs a key and no address; `BASE_URL` replaces its address if set.

```sh
DRUSITE__ASK__CHAT__PROVIDER=anthropic
DRUSITE__ASK__CHAT__MODEL=claude-sonnet-5-5
DRUSITE__ASK__CHAT__API_KEY_FILE=/run/secrets/anthropic
```

Embeddings and chat can be different providers: say, a small local embedding model and a vendor's
chat model, or the reverse. With no embedding model, Ask finds blocks by their words alone, which
works but misses blocks that say the same thing in other words.

| Setting | Default | |
|---|---|---|
| `DRUSITE__ASK__ENABLED` | `true` | `false` turns Ask off |
| `DRUSITE__ASK__CHAT__MAX_TOKENS` | `4096` | the longest answer; a reasoning model's thinking counts too |
| `DRUSITE__ASK__CHAT__TEMPERATURE` | the model's | |
| `DRUSITE__ASK__SOURCES` | `12` | the most blocks an answer draws on |
| `DRUSITE__ASK__MAX_CONTEXT_CHARS` | `24000` | the most text from pages sent with one question |
| `DRUSITE__ASK__HISTORY_TURNS` | `4` | earlier questions and answers sent with a follow-up |
| `DRUSITE__ASK__PER_MINUTE` / `PER_DAY` | `10` / `500` | questions one person may ask; two at once at most |
| `DRUSITE__ASK__TIMEOUT_SECS` | `300` | the longest an answer may take |
| `DRUSITE__ASK__IDLE_SECS` | `120` | the longest the model may go quiet once it has started answering |
| `DRUSITE__ASK__AUDIT_DAYS` | `90` | how long the record of what was sent is kept |

What to know before turning it on:

- **The provider sees the blocks it's sent.** Each question sends up to `SOURCES` blocks of the
  person's notes (and the question) to the chat model. With a vendor, that's the vendor; with a
  self-hosted model, it stays on your hardware. The question also goes to the embedding model.
- **Journal entries are left out** unless the person turns on Settings › Journal › "Ask your notes
  reads the journal". The setting syncs with the account.
- **Who can read what.** Only workspaces the person belongs to are searched, checked with the same
  Cedar policy and row-level security as everything else. Conversations are the asker's alone: other
  members of a workspace don't see them. Titles and messages are sealed with the workspace's key.
- **Text from pages is data, not instructions.** Each block goes to the model inside an element
  named with a random tag made for that question, which a page can't close, and the instructions say
  never to obey what a source says. The model has no tools, and the apps show answers as plain text
  with citations: nothing in an answer can load an image or follow a link.
- **What was sent where is recorded** in `ai_audit`: for each call that carried something from the
  notes (an answer, a question's or a search's vector, indexing), the feature, provider, host, model,
  the IDs of the blocks sent, how many characters and tokens, and how it went. Never the text.
  Workspace owners can read it at `GET /api/v1/workspaces/<id>/ai-audit`. The per-person limits
  count these records, so they hold across replicas.
- **No note text in the logs.** Provider errors are logged as a category (`unauthorized`,
  `rate_limited`, `network`, …), never with the provider's message, which can quote the request.
- **Reasoning models** (Qwen 3, DeepSeek R1 and the like) are fine: a `<think>` block at the start
  of an answer is held back, and reasoning sent in its own field is never read. Their thinking
  counts toward `MAX_TOKENS`; if answers come back empty, raise it.
- An answer is written by a task of its own on the server, so if the phone sleeps or the
  connection drops, it's still saved, and the conversation shows it when opened again.

To try it on your own machine with a small model:

```sh
docker run -d --name ollama -p 11434:11434 ollama/ollama
docker exec ollama ollama pull qwen3:0.6b
docker exec ollama ollama pull nomic-embed-text
DRUSITE__ASK__CHAT__BASE_URL=http://localhost:11434/v1 DRUSITE__ASK__CHAT__MODEL=qwen3:0.6b \
DRUSITE__SEARCH__EMBEDDINGS__BASE_URL=http://localhost:11434/v1 \
DRUSITE__SEARCH__EMBEDDINGS__MODEL=nomic-embed-text drusite-server
```

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
- whether it may edit journal entries (it can't unless you allow it);
- whether it may publish pages to the web (it can't unless you allow it).

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

## The web app

The full app in a browser, for computers where it can't be installed: the server serves it at
its own address (`public_url`), so there's nothing else to deploy. The image carries the built
app and turns it on (`[web] dir`); `[web] enabled = false` (the chart's `web.enabled`) turns it
off. Building the server yourself, point `dir` at `ui/dist-web` (`cd ui && npm run wasm &&
npm run build:web`).

It runs the same core as the apps, as WebAssembly, and keeps an encrypted copy of the notes in
the browser (OPFS), so it opens and edits offline and syncs when it's back. It signs in through
the server with every login method the server has on, and holds no token: the server keeps the
browser's sign-in, which is listed under Account → Devices like any device, and gives the
browser an HttpOnly, SameSite=Strict cookie that works only from the server's own pages.
Signing out, or signing the browser out from Devices, deletes its copy. Browsers stay signed in
for 14 days unused and 90 at most (`[auth] browser_idle_days`, `browser_max_days`).

What a browser can't do, the web app leaves out: the Markdown folder, agents on the same
computer (local MCP), AI providers kept on the device, and importing from Notion's API (Ask your
notes and importing a Notion export work). Whatever routes to the server must pass WebSockets
for sync, as for the apps.

## Publish to web

Anyone who can edit a page can share it as a read-only link, from the page's Share button. The
server renders the page as plain HTML, live: readers always see the page as it is now, and
unpublishing or making a new link takes the old address down at once. Links are long random
tokens, never page IDs. Each page's options are off until its editor turns them on: its
subpages (as a small site), search engines, an expiry date, and a password.

With no extra settings, published pages are at `https://<your server>/p/<token>`. That works,
but it means readers reach the same address as your app. To keep the app, its API and sign-in
private, give published pages **a host of their own**, which serves nothing but published pages
and their files (everything else there is a 404):

```toml
[publish]
public_url = "https://pages.example.com"
listen = "0.0.0.0:8443"   # its own port: published pages only
```

Then put only that port (or that host name) on the internet, by whichever of these suits you:

- **Behind any reverse proxy.** Route `pages.example.com` to port 8443 and nothing else; keep
  your app's host on your private network or VPN. If the proxy sets `X-Forwarded-For`, set
  `client_ip_header = "x-forwarded-for"` so the per-reader limits see readers rather than the
  proxy. Without `listen`, the main port also serves the public host to requests for its name;
  a separate port is easier to expose alone.
- **With the server's own TLS**, with no proxy: give it a certificate and key in PEM files.
  They're read again when they change, so a renewal (certbot, acme.sh, cert-manager) needs no
  restart.

  ```toml
  [publish]
  public_url = "https://pages.example.com"
  listen = "0.0.0.0:443"
  tls_cert = "/etc/drusite/tls/fullchain.pem"
  tls_key = "/etc/drusite/tls/privkey.pem"
  ```

- **Docker Compose**: uncomment `[publish]` in [`compose/drusite.toml`](compose/drusite.toml)
  and the `8443` port in [`compose/compose.yaml`](compose/compose.yaml).
- **Kubernetes**: the chart's `publish` values add the listener, a `public` Service port and,
  optionally, an Ingress for the public host alone, so it can use an internet-facing ingress
  class while the app's Ingress stays internal:

  ```yaml
  publish:
    enabled: true
    host: pages.example.com
    ingress:
      enabled: true
      className: public
      tls:
        - hosts: [pages.example.com]
          secretName: pages-tls
  ```

What readers get:

- Pages are rendered by the server, with no JavaScript, in light and dark, on phones too. Every
  piece of content is escaped, a strict CSP allows no script at all, `Referrer-Policy:
  no-referrer` keeps the link's token from leaking to the sites a page links to, links to pages
  that aren't shared become plain text, and no internal ID appears anywhere.
- A link that doesn't work (unknown, unpublished, expired, in the trash, a subpage that isn't
  shared) gets the same 404 as one that never existed.
- A page is `noindex` (in a meta tag and `X-Robots-Tag`) unless its editor allows search
  engines, and always while it has a password. `robots.txt` lets crawlers see that, and never
  lists links.
- A password page shows only a form. The password is hashed (argon2id), tries are limited (per
  address on each node, and per link across all of them), and a reader who knows it gets a
  signed cookie for that link, never a password in the URL. A new password or a new link ends
  those cookies.
- Pages carry an ETag and `Cache-Control: private, max-age=15`: a reader's browser may show a
  page up to that long after it's unpublished. Set `cache_secs` to change it.

Who may publish is policy (Cedar's `publish_document`): a workspace's editors and owners. An
agent publishes only if its person allowed that when connecting it, and its publishing shows in
the agents' activity. `enabled = false` turns publishing off (editors can still unpublish).

| Setting | Default | |
|---|---|---|
| `DRUSITE__PUBLISH__ENABLED` | `true` | `false`: no new links, and published pages are 404 |
| `DRUSITE__PUBLISH__PUBLIC_URL` | none | the public host's origin; links use it |
| `DRUSITE__PUBLISH__LISTEN` | none | the public host's own port |
| `DRUSITE__PUBLISH__TLS_CERT`, `DRUSITE__PUBLISH__TLS_KEY` | none | PEM files for TLS on that port |
| `DRUSITE__PUBLISH__CACHE_SECS` | `15` | how long browsers keep a page |
| `DRUSITE__PUBLISH__PER_MINUTE` | `300` | requests a minute from one address |
| `DRUSITE__PUBLISH__CLIENT_IP_HEADER` | none | where a proxy puts the reader's address |
