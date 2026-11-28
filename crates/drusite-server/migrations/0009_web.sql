-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The web app in a browser. It signs in like a device, and gets a grant like one (listed and
-- revocable with the others, with its own idle and absolute limits), but holds no token: the
-- server keeps the grant, and the browser gets an HttpOnly, SameSite=Strict cookie whose hash
-- finds it. Revoking or deleting the grant ends the cookie's session at once.
CREATE TABLE web_sessions (
    token_hash bytea PRIMARY KEY,
    grant_id uuid NOT NULL REFERENCES oauth_grants ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX web_sessions_grant ON web_sessions (grant_id);
