-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Publish to web: a page shared as a read-only link, rendered live by the server.

-- One live link per page. The link's token is a long random string, never the page's ID; it is
-- found by its hash and kept sealed with the workspace key so the person can copy the link
-- again. Making a new link replaces the token, so the old one stops working at once; deleting
-- the row unpublishes. The workspace document mirrors this row (without the password) so every
-- device shows it, but this row is what counts.
CREATE TABLE published_pages (
    page_id uuid PRIMARY KEY REFERENCES documents ON DELETE CASCADE,
    workspace_id uuid NOT NULL REFERENCES workspaces ON DELETE CASCADE,
    token_hash bytea NOT NULL UNIQUE,
    token bytea NOT NULL,
    include_subpages boolean NOT NULL DEFAULT false,
    allow_indexing boolean NOT NULL DEFAULT false,
    expires_at timestamptz,
    -- argon2id; never in the workspace document.
    password_phc text,
    -- Bumped whenever the password or the token changes, so a reader's cookie from before
    -- stops working.
    secret_version integer NOT NULL DEFAULT 0,
    -- Wrong passwords in the current window, for the per-link limit (shared by every replica).
    failed_unlocks integer NOT NULL DEFAULT 0,
    unlock_window_start timestamptz,
    published_by uuid REFERENCES users ON DELETE SET NULL,
    published_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX published_pages_workspace ON published_pages (workspace_id);

ALTER TABLE published_pages ENABLE ROW LEVEL SECURITY;
ALTER TABLE published_pages FORCE ROW LEVEL SECURITY;

-- Members see what's published in their workspace; editors and owners change it (Cedar's
-- publish_document decides first). Readers on the public host go through the system role,
-- found by the token's hash only.
CREATE POLICY published_pages_select ON published_pages FOR SELECT
    USING (drusite_is_system() OR drusite_role(workspace_id) IS NOT NULL);
CREATE POLICY published_pages_write ON published_pages FOR ALL
    USING (drusite_is_system() OR drusite_role(workspace_id) IN ('owner', 'editor'))
    WITH CHECK (drusite_is_system() OR drusite_role(workspace_id) IN ('owner', 'editor'));
