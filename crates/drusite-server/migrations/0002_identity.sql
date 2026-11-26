-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Identity: login credentials, browser sessions and Drusite's own OAuth 2.1 server.
--
-- None of this is workspace data and sign-in reads it before anyone is known, so these tables
-- have no row-level security; only the server's identity code touches them. Every token is
-- stored as a BLAKE3 hash, never as itself, so a copy of the database signs nobody in.

-- Password logins (argon2id, as a PHC string).
CREATE TABLE password_credentials (
    user_id uuid PRIMARY KEY REFERENCES users ON DELETE CASCADE,
    phc text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- TOTP (RFC 6238). Unconfirmed until the user has entered one code. `last_step` stops a code
-- being used twice.
CREATE TABLE totp_credentials (
    user_id uuid PRIMARY KEY REFERENCES users ON DELETE CASCADE,
    secret bytea NOT NULL,
    confirmed_at timestamptz,
    last_step bigint NOT NULL DEFAULT 0
);

-- Passkeys (WebAuthn), as webauthn-rs serialises them. `id` is the credential ID.
CREATE TABLE passkeys (
    id bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    name text NOT NULL DEFAULT '',
    passkey jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz
);
CREATE INDEX passkeys_user ON passkeys (user_id);

-- Accounts at OIDC providers. An account is found by (provider, subject), never by email.
CREATE TABLE oidc_identities (
    provider text NOT NULL,
    subject text NOT NULL,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    email text,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, subject)
);
CREATE INDEX oidc_identities_user ON oidc_identities (user_id);

-- Magic links. `user_id` is NULL when the link will create the account (self-signup).
CREATE TABLE login_tokens (
    token_hash bytea PRIMARY KEY,
    email text NOT NULL,
    user_id uuid REFERENCES users ON DELETE CASCADE,
    auth_request uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at timestamptz
);
CREATE INDEX login_tokens_email ON login_tokens (lower(email), created_at);

-- Short-lived state a sign-in carries between requests, so any server node can finish what
-- another started: OAuth authorization requests, WebAuthn and OIDC challenges, a password
-- login waiting for its TOTP code. `binding` is the hash of a cookie set in the browser that
-- started it, where the flow must finish in that same browser.
CREATE TABLE auth_challenges (
    id uuid PRIMARY KEY,
    kind text NOT NULL,
    data jsonb NOT NULL,
    binding bytea,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX auth_challenges_expiry ON auth_challenges (expires_at);

-- Signed-in browsers: the sign-in pages and account settings. Apps use OAuth tokens instead.
CREATE TABLE browser_sessions (
    token_hash bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    method text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX browser_sessions_user ON browser_sessions (user_id);

-- A device's sign-in: what an app gets when it signs in, bound to the device's DPoP key. Its
-- refresh tokens rotate on every use; revoking the grant signs the device out.
CREATE TABLE oauth_grants (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    client_id text NOT NULL,
    scope text NOT NULL,
    dpop_jkt text NOT NULL,
    auth_method text NOT NULL,
    device_name text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz NOT NULL DEFAULT now(),
    -- Signed out after this long unused, and after `absolute_expires_at` whatever happens.
    idle_expires_at timestamptz NOT NULL,
    absolute_expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    revoked_reason text
);
CREATE INDEX oauth_grants_user ON oauth_grants (user_id);

-- Authorization codes: one use, a minute or so.
CREATE TABLE oauth_codes (
    code_hash bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    client_id text NOT NULL,
    redirect_uri text NOT NULL,
    code_challenge text NOT NULL,
    scope text NOT NULL,
    dpop_jkt text,
    auth_method text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at timestamptz,
    -- The grant the code was exchanged for, revoked if the code is ever presented again.
    grant_id uuid REFERENCES oauth_grants ON DELETE SET NULL
);

CREATE TABLE refresh_tokens (
    token_hash bytea PRIMARY KEY,
    grant_id uuid NOT NULL REFERENCES oauth_grants ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    used_at timestamptz
);
CREATE INDEX refresh_tokens_grant ON refresh_tokens (grant_id);

-- Opaque access tokens: looked up on every connection, so revoking one takes effect at once.
CREATE TABLE access_tokens (
    token_hash bytea PRIMARY KEY,
    grant_id uuid NOT NULL REFERENCES oauth_grants ON DELETE CASCADE,
    expires_at timestamptz NOT NULL
);
CREATE INDEX access_tokens_grant ON access_tokens (grant_id);
CREATE INDEX access_tokens_expiry ON access_tokens (expires_at);

-- DPoP proofs seen recently, so none can be replayed (RFC 9449 §11.1).
CREATE TABLE dpop_proofs (
    jkt text NOT NULL,
    jti text NOT NULL,
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (jkt, jti)
);
CREATE INDEX dpop_proofs_expiry ON dpop_proofs (expires_at);

-- Failed sign-ins and sent emails, for rate limits. `key` names what is limited, such as
-- `password:<email>`.
CREATE TABLE auth_attempts (
    key text NOT NULL,
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX auth_attempts_key ON auth_attempts (key, at);
