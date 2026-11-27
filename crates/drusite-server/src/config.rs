// SPDX-License-Identifier: AGPL-3.0-or-later

//! The server's configuration: one TOML file, every value overridable by an environment
//! variable, and any value readable from a file so secrets never sit in the config.
//!
//! - `DRUSITE__AUTH__PASSWORD__ENABLED=true` sets `[auth.password] enabled`. Sections and keys are
//!   separated by a double underscore and matched in lowercase. `true`, `false`, numbers and
//!   `[...]` arrays are read as TOML; anything else is a string (quote it, `'"123"'`, to force a
//!   string).
//! - `password_file = "/run/secrets/smtp"` sets `password` to that file's contents, with the
//!   trailing newline removed. This works for every key, in the file or from the environment
//!   (`DRUSITE__SMTP__PASSWORD_FILE=...`).
//!
//! Nothing here assumes a particular deployment: any S3-compatible store, any PostgreSQL 16+,
//! any OIDC provider, any OTLP backend.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};
use url::Url;
use zeroize::Zeroizing;

/// The prefix of environment variables that override the config file.
pub const ENV_PREFIX: &str = "DRUSITE__";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("can't read {path}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} isn't valid TOML")]
    Parse {
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    #[error("environment variable {var}: {reason}")]
    Env { var: String, reason: String },
    #[error("{key} and {key}_file are both set; use one")]
    BothValueAndFile { key: String },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// A secret value: never printed, wiped from memory when dropped.
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // A numeric secret from the environment arrives as a TOML integer.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            String(String),
            Int(i64),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::String(s) => Self::new(s),
            Raw::Int(i) => Self::new(i.to_string()),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where users and apps reach this server, e.g. `https://notes.example.com`. Links in emails,
    /// OAuth redirects and passkey origins are built from it.
    pub public_url: Url,
    #[serde(default)]
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub storage: StorageConfig,
    pub keys: KeysConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub smtp: Option<SmtpConfig>,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub attachments: AttachmentsConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
}

/// Agents (MCP clients) reading and writing notes at `/mcp`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    /// Agents can connect. On unless set.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// For development and tests only: accept client ID metadata documents served over plain
    /// HTTP, or from loopback and private addresses.
    #[serde(default)]
    pub allow_insecure_client_ids: bool,
    /// The longest an agent token made on the account page may last, in days.
    #[serde(default = "default_agent_token_days")]
    pub max_token_days: u32,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_insecure_client_ids: false,
            max_token_days: default_agent_token_days(),
        }
    }
}

const fn default_agent_token_days() -> u32 {
    365
}

/// Files on pages.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentsConfig {
    /// The largest file a page can hold, in bytes. 100 MiB unless set.
    #[serde(default = "default_max_attachment")]
    pub max_bytes: u64,
}

impl Default for AttachmentsConfig {
    fn default() -> Self {
        Self {
            max_bytes: default_max_attachment(),
        }
    }
}

const fn default_max_attachment() -> u64 {
    100 * 1024 * 1024
}

/// Search over the live documents: Postgres full-text search, plus semantic search when an
/// embedding model is configured.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    /// Off keeps even derived data (words and embeddings) out of the database.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The PostgreSQL text search configuration: `simple` (any language, no stemming) or a
    /// language's, such as `english`.
    #[serde(default = "default_text_config")]
    pub text_config: String,
    #[serde(default)]
    pub embeddings: Option<EmbeddingsConfig>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            text_config: default_text_config(),
            embeddings: None,
        }
    }
}

fn default_text_config() -> String {
    "simple".to_owned()
}

/// The embedding model search indexes with, on any provider that makes embeddings: OpenAI,
/// Google, Mistral, OpenRouter, or an OpenAI-compatible endpoint (LiteLLM, Ollama, vLLM, …).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingsConfig {
    /// `openai`, `google`, `mistral`, `openrouter` or `openai-compatible` (the default).
    #[serde(default = "default_embeddings_provider")]
    pub provider: drusite_ai::Kind,
    /// The API's base URL, e.g. `http://litellm:4000/v1`. Needed for `openai-compatible`; for a
    /// vendor it replaces the vendor's own address (a proxy, a regional endpoint).
    #[serde(default)]
    pub base_url: Option<Url>,
    pub model: String,
    /// Names the vector space stored with every vector; the model name unless set. Changing it
    /// (or the model) embeds everything again.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub api_key: Option<Secret>,
    /// Asks for vectors of this size, for models that can shorten them.
    #[serde(default)]
    pub dimensions: Option<u32>,
    /// How many blocks go in one request.
    #[serde(default = "default_embedding_batch")]
    pub batch: usize,
    /// How close in meaning (cosine similarity, -1 to 1) a block must be to count as a match.
    /// Nearest neighbours always exist; this keeps unrelated ones out. Models differ: raise it
    /// if searches return unrelated pages, lower it if they miss related ones.
    #[serde(default = "default_min_similarity")]
    pub min_similarity: f64,
}

const fn default_embeddings_provider() -> drusite_ai::Kind {
    drusite_ai::Kind::OpenAiCompatible
}

const fn default_min_similarity() -> f64 {
    0.3
}

const fn default_embedding_batch() -> usize {
    64
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Longest a plain HTTP request may take. WebSockets aren't subject to it.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            request_timeout_secs: default_request_timeout(),
        }
    }
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 8080))
}

const fn default_request_timeout() -> u64 {
    30
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// A PostgreSQL URL, e.g. `postgres://drusite@db/drusite`. Usually from `url_file` or
    /// `DRUSITE__DATABASE__URL`, since it carries the password.
    pub url: Secret,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    /// Apply pending migrations when the server starts. Turn off to run `drusite-server migrate`
    /// as a separate step instead.
    #[serde(default = "yes")]
    pub migrate_on_start: bool,
    /// Row-level security is Drusite's second line between workspaces, and a superuser or a role
    /// with BYPASSRLS skips it. The server refuses such a role unless this is set.
    #[serde(default)]
    pub allow_rls_bypass: bool,
}

const fn default_max_connections() -> u32 {
    16
}

const fn yes() -> bool {
    true
}

/// Any S3-compatible object store: AWS S3, Cloudflare R2, Garage, SeaweedFS, Ceph, …
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub bucket: String,
    /// The store's endpoint. Leave out for AWS S3.
    #[serde(default)]
    pub endpoint: Option<Url>,
    #[serde(default = "default_region")]
    pub region: String,
    /// Credentials. Leave both out to use the standard `AWS_*` variables, a web identity token or
    /// instance metadata.
    #[serde(default)]
    pub access_key_id: Option<Secret>,
    #[serde(default)]
    pub secret_access_key: Option<Secret>,
    /// `https://bucket.endpoint/key` instead of `https://endpoint/bucket/key`. Most self-hosted
    /// stores want path style, the default.
    #[serde(default)]
    pub virtual_hosted_style: bool,
    /// A prefix for every object key, to share a bucket.
    #[serde(default)]
    pub prefix: String,
}

fn default_region() -> String {
    "us-east-1".to_owned()
}

/// Where the master key comes from. Each workspace's own key is wrapped by it.
#[derive(Debug, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum KeysConfig {
    /// Master keys in an environment variable (`DRUSITE_MASTER_KEY` unless `var` says otherwise).
    Env {
        #[serde(default = "default_master_key_var")]
        var: String,
    },
    /// Master keys in a file, such as a Kubernetes or Docker secret.
    File { path: PathBuf },
    /// A key management service that wraps and unwraps workspace keys itself, so the master key
    /// never reaches Drusite: the Transit engine of OpenBao or HashiCorp Vault.
    Transit {
        address: Url,
        /// The Transit key's name.
        key: String,
        #[serde(default = "default_transit_mount")]
        mount: String,
        token: Secret,
        /// A Vault Enterprise / OpenBao namespace.
        #[serde(default)]
        namespace: Option<String>,
    },
}

fn default_master_key_var() -> String {
    "DRUSITE_MASTER_KEY".to_owned()
}

fn default_transit_mount() -> String {
    "transit".to_owned()
}

/// Every login method is a switch. A method that's off has no routes at all.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// Let anyone create an account with a password, a passkey or a magic link. OIDC providers
    /// decide for themselves (`auto_create`). Off by default: accounts come from an OIDC provider
    /// or `drusite-server account create`.
    #[serde(default)]
    pub self_signup: bool,
    #[serde(default)]
    pub password: PasswordConfig,
    #[serde(default)]
    pub passkeys: PasskeysConfig,
    #[serde(default)]
    pub magic_link: MagicLinkConfig,
    /// OIDC providers by ID (lowercase letters, digits and `_`), e.g. `[auth.oidc.authentik]`.
    #[serde(default)]
    pub oidc: BTreeMap<String, OidcProviderConfig>,
    /// How long an app's access token lasts. Apps refresh it with their device key.
    #[serde(default = "default_access_token_minutes")]
    pub access_token_minutes: u32,
    /// A device that hasn't connected for this long is signed out.
    #[serde(default = "default_device_idle_days")]
    pub device_idle_days: u32,
    /// Every device signs in again after this long, however often it connects.
    #[serde(default = "default_device_max_days")]
    pub device_max_days: u32,
    /// How long a browser stays signed in to the sign-in pages and account settings.
    #[serde(default = "default_session_hours")]
    pub session_hours: u32,
}

const fn default_access_token_minutes() -> u32 {
    10
}

const fn default_device_idle_days() -> u32 {
    30
}

const fn default_device_max_days() -> u32 {
    365
}

const fn default_session_hours() -> u32 {
    12
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            self_signup: false,
            password: PasswordConfig::default(),
            passkeys: PasskeysConfig::default(),
            magic_link: MagicLinkConfig::default(),
            oidc: BTreeMap::new(),
            access_token_minutes: default_access_token_minutes(),
            device_idle_days: default_device_idle_days(),
            device_max_days: default_device_max_days(),
            session_hours: default_session_hours(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Every password account must also set up TOTP.
    #[serde(default)]
    pub require_totp: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasskeysConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicLinkConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_magic_link_ttl")]
    pub ttl_minutes: u32,
}

impl Default for MagicLinkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ttl_minutes: default_magic_link_ttl(),
        }
    }
}

const fn default_magic_link_ttl() -> u32 {
    15
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcProviderConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The name on the sign-in button, e.g. "Authentik" or "Google".
    pub display_name: String,
    /// The issuer exactly as the provider states it. Providers compare it character for
    /// character, trailing slash included (Authentik's and Auth0's have one, Keycloak's and
    /// Google's don't), so it stays a string rather than a parsed URL, which would add or keep
    /// a slash on its own.
    pub issuer: String,
    pub client_id: String,
    /// Leave out for a public client.
    #[serde(default)]
    pub client_secret: Option<Secret>,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    /// Create an account on someone's first sign-in through this provider.
    #[serde(default)]
    pub auto_create: bool,
    /// Let a first sign-in through this provider join the existing account with the same
    /// email. Only when the provider says it has verified the email (`email_verified`), and only
    /// for a provider you trust to verify emails properly: anyone who can make an account there
    /// with your email could otherwise take over your Drusite account. Off by default.
    #[serde(default)]
    pub link_by_verified_email: bool,
    /// Only people whose verified email is in one of these domains may sign in through this
    /// provider (empty: any).
    #[serde(default)]
    pub allowed_email_domains: Vec<String>,
    /// Only members of these provider groups may sign in through this provider (empty: any).
    /// Checked at every sign-in, so removing someone from the group locks them out.
    #[serde(default)]
    pub allowed_groups: Vec<String>,
    /// The ID-token claim that lists the user's groups.
    #[serde(default = "default_groups_claim")]
    pub groups_claim: String,
}

fn default_scopes() -> Vec<String> {
    ["openid", "email", "profile"].map(str::to_owned).to_vec()
}

fn default_groups_claim() -> String {
    "groups".to_owned()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<Secret>,
    /// The sender, e.g. `Drusite <notes@example.com>`.
    pub from: String,
    #[serde(default)]
    pub tls: SmtpTls,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmtpTls {
    /// TLS from the first byte (port 465).
    Tls,
    /// Plain connection upgraded with STARTTLS (port 587). Fails if the server can't.
    #[default]
    Starttls,
    /// No encryption, for a relay on the same host or network only.
    None,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    /// `service.name` in traces, metrics and logs.
    #[serde(default = "default_service_name")]
    pub service_name: String,
    /// Logs on stderr: `json` for log collectors, `pretty` for people.
    #[serde(default)]
    pub log_format: LogFormat,
    /// Send traces, metrics and logs over OTLP/HTTP. On when the standard
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` is set, unless this says otherwise; the endpoint, headers
    /// and the rest come from the standard `OTEL_*` variables.
    #[serde(default)]
    pub otlp: Option<bool>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            service_name: default_service_name(),
            log_format: LogFormat::default(),
            otlp: None,
        }
    }
}

fn default_service_name() -> String {
    "drusite-server".to_owned()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    #[default]
    Json,
    Pretty,
}

/// One login method, as the apps see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginMethod {
    Password,
    Passkeys,
    MagicLink,
    Oidc { id: String, display_name: String },
}

impl AuthConfig {
    /// The login methods that are switched on.
    pub fn enabled_methods(&self) -> Vec<LoginMethod> {
        let mut methods = Vec::new();
        if self.passkeys.enabled {
            methods.push(LoginMethod::Passkeys);
        }
        for (id, provider) in &self.oidc {
            if provider.enabled {
                methods.push(LoginMethod::Oidc {
                    id: id.clone(),
                    display_name: provider.display_name.clone(),
                });
            }
        }
        if self.password.enabled {
            methods.push(LoginMethod::Password);
        }
        if self.magic_link.enabled {
            methods.push(LoginMethod::MagicLink);
        }
        methods
    }
}

impl Config {
    /// Reads the config file (if any), applies `DRUSITE__*` environment overrides and `*_file`
    /// values, and checks the result.
    pub fn load(
        path: Option<&Path>,
        env: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, ConfigError> {
        let mut table = match path {
            Some(path) => {
                let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
                    path: path.to_owned(),
                    source,
                })?;
                text.parse::<toml::Table>()
                    .map_err(|source| ConfigError::Parse {
                        path: path.to_owned(),
                        source: Box::new(source),
                    })?
            }
            None => toml::Table::new(),
        };
        apply_env(&mut table, env)?;
        resolve_files(&mut table, "")?;
        let config: Self = toml::Value::Table(table)
            .try_into()
            .map_err(|e: toml::de::Error| ConfigError::Invalid(e.message().to_owned()))?;
        config.validate()?;
        Ok(config)
    }

    /// Checks what serde can't: that at least one login method is on, and that each one has
    /// what it needs.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |msg: String| Err(ConfigError::Invalid(msg));

        match self.public_url.scheme() {
            "https" => {}
            "http" if is_loopback(&self.public_url) => {}
            "http" => {
                return invalid(format!(
                    "public_url {} must use https (plain http is allowed only for localhost)",
                    self.public_url
                ));
            }
            other => return invalid(format!("public_url has unsupported scheme {other:?}")),
        }

        let search = &self.search;
        if search.text_config.is_empty()
            || !search
                .text_config
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_')
        {
            return invalid(format!(
                "[search] text_config {:?} isn't a text search configuration name",
                search.text_config
            ));
        }
        if let Some(e) = &search.embeddings
            && (e.model.trim().is_empty()
                || e.batch == 0
                || !(-1.0..=1.0).contains(&e.min_similarity))
        {
            return invalid(
                "[search.embeddings] needs a model, a batch of at least 1 and a min_similarity \
                 between -1 and 1"
                    .into(),
            );
        }
        if let Some(e) = &search.embeddings {
            if !e.provider.embeds() {
                return invalid(format!(
                    "[search.embeddings] provider {} doesn't make embeddings",
                    e.provider.label()
                ));
            }
            if e.base_url.is_none() && e.provider.default_base().is_none() {
                return invalid(
                    "[search.embeddings] base_url is needed for an openai-compatible provider"
                        .into(),
                );
            }
            if e.provider.needs_key() && e.api_key.is_none() {
                return invalid(format!(
                    "[search.embeddings] api_key is needed for {}",
                    e.provider.label()
                ));
            }
        }

        let auth = &self.auth;
        if auth.enabled_methods().is_empty() {
            return invalid(
                "no login method is enabled. Turn on at least one of [auth.password], \
                 [auth.passkeys], [auth.magic_link] or an [auth.oidc.<id>] provider. To get into \
                 an account without one, use `drusite-server account recover` on the server."
                    .to_owned(),
            );
        }
        if auth.passkeys.enabled && !matches!(self.public_url.host(), Some(url::Host::Domain(_))) {
            return invalid(format!(
                "[auth.passkeys] needs public_url to use a domain name, not an IP address \
                 ({}): passkeys belong to a domain. Use localhost for local testing.",
                self.public_url
            ));
        }
        if auth.magic_link.enabled && self.smtp.is_none() {
            return invalid("[auth.magic_link] is enabled but [smtp] isn't configured".to_owned());
        }
        if auth.magic_link.enabled && !(1..=24 * 60).contains(&auth.magic_link.ttl_minutes) {
            return invalid("[auth.magic_link] ttl_minutes must be 1 to 1440".to_owned());
        }
        for (id, provider) in &auth.oidc {
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                return invalid(format!(
                    "OIDC provider ID {id:?} must be lowercase letters, digits and _"
                ));
            }
            let issuer = Url::parse(&provider.issuer).map_err(|e| {
                ConfigError::Invalid(format!("[auth.oidc.{id}] issuer isn't a URL: {e}"))
            })?;
            if issuer.scheme() != "https" && !is_loopback(&issuer) {
                return invalid(format!("[auth.oidc.{id}] issuer must use https"));
            }
            if !provider.scopes.iter().any(|s| s == "openid") {
                return invalid(format!("[auth.oidc.{id}] scopes must include openid"));
            }
            if !provider.allowed_groups.is_empty() && provider.groups_claim.is_empty() {
                return invalid(format!(
                    "[auth.oidc.{id}] allowed_groups needs groups_claim"
                ));
            }
        }

        if self.storage.bucket.is_empty() {
            return invalid("[storage] bucket is empty".to_owned());
        }
        if self.storage.access_key_id.is_some() != self.storage.secret_access_key.is_some() {
            return invalid(
                "[storage] set both access_key_id and secret_access_key, or neither".to_owned(),
            );
        }
        if let Some(smtp) = &self.smtp
            && smtp.username.is_some() != smtp.password.is_some()
        {
            return invalid("[smtp] set both username and password, or neither".to_owned());
        }
        if !(1..=60).contains(&auth.access_token_minutes) {
            return invalid("[auth] access_token_minutes must be 1 to 60".to_owned());
        }
        if auth.device_idle_days == 0 || auth.device_max_days < auth.device_idle_days {
            return invalid(
                "[auth] device_idle_days must be at least 1 and device_max_days at least that"
                    .to_owned(),
            );
        }
        if !(1..=24 * 30).contains(&auth.session_hours) {
            return invalid("[auth] session_hours must be 1 to 720".to_owned());
        }
        if self.database.max_connections == 0 {
            return invalid("[database] max_connections must be at least 1".to_owned());
        }
        Ok(())
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d == "localhost" || d.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Applies `DRUSITE__SECTION__KEY=value` overrides.
fn apply_env(
    table: &mut toml::Table,
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<(), ConfigError> {
    // Sorted, so the outcome doesn't depend on the environment's order.
    let mut vars: Vec<_> = env
        .into_iter()
        .filter(|(k, _)| k.starts_with(ENV_PREFIX))
        .collect();
    vars.sort();
    for (var, raw) in vars {
        let path: Vec<String> = var[ENV_PREFIX.len()..]
            .split("__")
            .map(str::to_ascii_lowercase)
            .collect();
        if path.iter().any(String::is_empty) {
            return Err(ConfigError::Env {
                var,
                reason: "empty section or key".to_owned(),
            });
        }
        let value = env_value(&raw);
        let (last, parents) = path.split_last().expect("split gives at least one part");
        let mut cursor = &mut *table;
        for part in parents {
            let entry = cursor
                .entry(part.clone())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            cursor = match entry {
                toml::Value::Table(t) => t,
                _ => {
                    return Err(ConfigError::Env {
                        var,
                        reason: format!("{part} isn't a section"),
                    });
                }
            };
        }
        cursor.insert(last.clone(), value);
    }
    Ok(())
}

/// `true`, `false`, numbers, arrays, inline tables and quoted strings are TOML; anything else is
/// a plain string.
fn env_value(raw: &str) -> toml::Value {
    let trimmed = raw.trim();
    let looks_typed = matches!(trimmed, "true" | "false")
        || trimmed.starts_with(['[', '{', '"', '\''])
        || trimmed.parse::<i64>().is_ok()
        || (trimmed.parse::<f64>().is_ok() && trimmed.contains('.'));
    if looks_typed
        && let Ok(mut t) = format!("v = {trimmed}").parse::<toml::Table>()
        && let Some(v) = t.remove("v")
    {
        return v;
    }
    toml::Value::String(raw.to_owned())
}

/// Replaces every `key_file = "path"` with `key = <the file's contents>`.
fn resolve_files(table: &mut toml::Table, at: &str) -> Result<(), ConfigError> {
    let file_keys: Vec<String> = table
        .keys()
        .filter(|k| k.ends_with("_file") && k.len() > "_file".len())
        .cloned()
        .collect();
    for file_key in file_keys {
        let key = file_key.trim_end_matches("_file").to_owned();
        let full = if at.is_empty() {
            key.clone()
        } else {
            format!("{at}.{key}")
        };
        if table.contains_key(&key) {
            return Err(ConfigError::BothValueAndFile { key: full });
        }
        let Some(toml::Value::String(path)) = table.remove(&file_key) else {
            return Err(ConfigError::Invalid(format!("{full}_file must be a path")));
        };
        let path = PathBuf::from(path);
        let contents =
            std::fs::read_to_string(&path).map_err(|source| ConfigError::Read { path, source })?;
        let contents = contents.trim_end_matches(['\n', '\r']).to_owned();
        table.insert(key, toml::Value::String(contents));
    }
    for (key, value) in table.iter_mut() {
        if let toml::Value::Table(inner) = value {
            let path = if at.is_empty() {
                key.clone()
            } else {
                format!("{at}.{key}")
            };
            resolve_files(inner, &path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        public_url = "https://notes.example.com"
        [database]
        url = "postgres://drusite@localhost/drusite"
        [storage]
        bucket = "drusite"
        endpoint = "http://localhost:8333"
        [keys]
        source = "env"
    "#;

    fn load(toml: &str, env: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drusite.toml");
        std::fs::write(&path, toml).unwrap();
        Config::load(
            Some(&path),
            env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        )
    }

    #[test]
    fn refuses_to_start_with_no_login_method() {
        let err = load(MINIMAL, &[]).unwrap_err();
        assert!(
            err.to_string().contains("no login method is enabled"),
            "{err}"
        );
        // An OIDC provider that's switched off doesn't count.
        let toml = format!(
            "{MINIMAL}\n[auth.oidc.idp]\nenabled = false\ndisplay_name = \"IdP\"\n\
             issuer = \"https://idp.example.com\"\nclient_id = \"drusite\"\n"
        );
        assert!(load(&toml, &[]).is_err());
    }

    #[test]
    fn embeddings_take_any_provider_that_makes_them() {
        let env = |extra: &[(&'static str, &'static str)]| {
            let mut v = vec![
                ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
                ("DRUSITE__SEARCH__EMBEDDINGS__MODEL", "m"),
            ];
            v.extend_from_slice(extra);
            v
        };
        // A custom endpoint, as before: the default provider.
        let c = load(
            MINIMAL,
            &env(&[(
                "DRUSITE__SEARCH__EMBEDDINGS__BASE_URL",
                "http://litellm:4000/v1",
            )]),
        )
        .unwrap();
        let e = c.search.embeddings.unwrap();
        assert_eq!(e.provider, drusite_ai::Kind::OpenAiCompatible);
        // A vendor, with a key and no address.
        let c = load(
            MINIMAL,
            &env(&[
                ("DRUSITE__SEARCH__EMBEDDINGS__PROVIDER", "google"),
                ("DRUSITE__SEARCH__EMBEDDINGS__API_KEY", "g"),
            ]),
        )
        .unwrap();
        assert_eq!(
            c.search.embeddings.unwrap().provider,
            drusite_ai::Kind::Google
        );
        // What can't work is refused at start.
        for (extra, says) in [
            (vec![], "base_url is needed"),
            (
                vec![("DRUSITE__SEARCH__EMBEDDINGS__PROVIDER", "openai")],
                "api_key is needed",
            ),
            (
                vec![
                    ("DRUSITE__SEARCH__EMBEDDINGS__PROVIDER", "anthropic"),
                    ("DRUSITE__SEARCH__EMBEDDINGS__API_KEY", "a"),
                ],
                "doesn't make embeddings",
            ),
        ] {
            let err = load(MINIMAL, &env(&extra)).unwrap_err();
            assert!(err.to_string().contains(says), "{err}");
        }
    }

    #[test]
    fn environment_overrides_the_file() {
        let config = load(
            MINIMAL,
            &[
                ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
                ("DRUSITE__DATABASE__MAX_CONNECTIONS", "4"),
                ("DRUSITE__STORAGE__BUCKET", "other"),
                ("DRUSITE__AUTH__OIDC__AUTHENTIK__DISPLAY_NAME", "Authentik"),
                (
                    "DRUSITE__AUTH__OIDC__AUTHENTIK__ISSUER",
                    "https://auth.example.com/application/o/drusite/",
                ),
                ("DRUSITE__AUTH__OIDC__AUTHENTIK__CLIENT_ID", "abc"),
                (
                    "DRUSITE__AUTH__OIDC__AUTHENTIK__ALLOWED_GROUPS",
                    "[\"notes\"]",
                ),
                ("UNRELATED", "x"),
            ],
        )
        .unwrap();
        assert!(config.auth.password.enabled);
        assert_eq!(config.database.max_connections, 4);
        assert_eq!(config.storage.bucket, "other");
        let oidc = &config.auth.oidc["authentik"];
        assert_eq!(oidc.allowed_groups, ["notes"]);
        assert!(oidc.enabled);
        assert_eq!(
            config.auth.enabled_methods(),
            [
                LoginMethod::Oidc {
                    id: "authentik".into(),
                    display_name: "Authentik".into()
                },
                LoginMethod::Password
            ]
        );
    }

    #[test]
    fn secrets_come_from_files() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("db-url");
        std::fs::write(&secret, "postgres://u:p@db/drusite\n").unwrap();
        let toml = MINIMAL.replace(
            "url = \"postgres://drusite@localhost/drusite\"",
            &format!("url_file = {:?}", secret.display().to_string()),
        );
        let config = load(&toml, &[("DRUSITE__AUTH__PASSKEYS__ENABLED", "true")]).unwrap();
        assert_eq!(config.database.url.expose(), "postgres://u:p@db/drusite");
        assert!(
            !format!("{config:?}").contains("u:p@"),
            "secrets are redacted"
        );

        let both = load(
            &toml,
            &[
                ("DRUSITE__AUTH__PASSKEYS__ENABLED", "true"),
                ("DRUSITE__DATABASE__URL", "postgres://x"),
            ],
        )
        .unwrap_err();
        assert!(
            matches!(both, ConfigError::BothValueAndFile { .. }),
            "{both}"
        );
    }

    #[test]
    fn numeric_secret_from_the_environment_is_a_string() {
        let config = load(
            MINIMAL,
            &[
                ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
                ("DRUSITE__STORAGE__ACCESS_KEY_ID", "123456"),
                ("DRUSITE__STORAGE__SECRET_ACCESS_KEY", "654321"),
            ],
        )
        .unwrap();
        assert_eq!(
            config.storage.access_key_id.as_ref().unwrap().expose(),
            "123456"
        );
    }

    #[test]
    fn checks_each_method_has_what_it_needs() {
        let err = load(MINIMAL, &[("DRUSITE__AUTH__MAGIC_LINK__ENABLED", "true")]).unwrap_err();
        assert!(err.to_string().contains("[smtp]"), "{err}");

        let insecure = MINIMAL.replace("https://notes.example.com", "http://notes.example.com");
        let err = load(&insecure, &[("DRUSITE__AUTH__PASSWORD__ENABLED", "true")]).unwrap_err();
        assert!(err.to_string().contains("https"), "{err}");
        let local = MINIMAL.replace("https://notes.example.com", "http://localhost:8080");
        assert!(load(&local, &[("DRUSITE__AUTH__PASSWORD__ENABLED", "true")]).is_ok());

        let bad_id = format!(
            "{MINIMAL}\n[auth.oidc.My-IdP]\ndisplay_name = \"x\"\n\
             issuer = \"https://idp.example.com\"\nclient_id = \"c\"\n"
        );
        assert!(load(&bad_id, &[]).is_err());
    }

    #[test]
    fn the_example_config_is_valid() {
        // Its secrets point at files that exist only in a deployment; use inline values.
        let example = include_str!("../drusite.example.toml")
            .lines()
            .map(|line| match line.split_once("_file = ") {
                Some((key, _)) if !line.starts_with('#') => format!("{key} = \"secret\""),
                _ => line.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let config = load(&example, &[]).unwrap();
        assert_eq!(config.auth.enabled_methods().len(), 2);
    }

    #[test]
    fn unknown_keys_are_errors() {
        let err = load(
            MINIMAL,
            &[
                ("DRUSITE__AUTH__PASSWORD__ENABLED", "true"),
                ("DRUSITE__AUTH__PASSWORD__ENABELD", "true"),
            ],
        )
        .unwrap_err();
        assert!(err.to_string().contains("enabeld"), "{err}");
    }
}
