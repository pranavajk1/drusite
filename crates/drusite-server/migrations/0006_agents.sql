-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Agents: MCP clients signed in to read and write notes at /mcp, and what they did.

-- An agent's sign-in is a grant like a device's, for another resource (RFC 8707): the MCP
-- endpoint's URL. Its tokens are good there and nowhere else, and the app's are no good there.
-- Most agents can't do DPoP, so their tokens may be bearer tokens.
ALTER TABLE oauth_grants ALTER COLUMN dpop_jkt DROP NOT NULL;
ALTER TABLE oauth_grants ADD COLUMN resource text;
-- What the agent may do (drusite_mcp::Access as JSON); NULL for the app's own sign-ins.
ALTER TABLE oauth_grants ADD COLUMN access jsonb;
-- The name the agent gave (its client ID metadata document's client_name) or the person gave.
ALTER TABLE oauth_grants ADD COLUMN client_name text NOT NULL DEFAULT '';

ALTER TABLE oauth_codes ADD COLUMN resource text;
ALTER TABLE oauth_codes ADD COLUMN access jsonb;
ALTER TABLE oauth_codes ADD COLUMN client_name text NOT NULL DEFAULT '';

-- Every change an agent made, so the person can see it and undo it. The summary names pages,
-- so it's sealed with the workspace's key like the documents are; `effects` holds only page
-- IDs and versions.
CREATE TABLE agent_activity (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    -- Not a foreign key: the record outlives the sign-in.
    grant_id uuid NOT NULL,
    agent_name text NOT NULL,
    tool text NOT NULL,
    summary bytea NOT NULL,
    effects jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    undone_at timestamptz,
    undone_by uuid REFERENCES users ON DELETE SET NULL
);
CREATE INDEX agent_activity_recent ON agent_activity (workspace_id, created_at DESC);

ALTER TABLE agent_activity ENABLE ROW LEVEL SECURITY;
ALTER TABLE agent_activity FORCE ROW LEVEL SECURITY;

CREATE POLICY agent_activity_select ON agent_activity FOR SELECT
    USING (drusite_is_system() OR drusite_role(workspace_id) IS NOT NULL);
CREATE POLICY agent_activity_insert ON agent_activity FOR INSERT
    WITH CHECK (
        drusite_is_system()
        OR (user_id = drusite_user_id() AND drusite_role(workspace_id) IN ('owner', 'editor'))
    );
-- Only marking it undone; the record itself never changes.
CREATE POLICY agent_activity_update ON agent_activity FOR UPDATE
    USING (drusite_is_system() OR drusite_role(workspace_id) IN ('owner', 'editor'));
