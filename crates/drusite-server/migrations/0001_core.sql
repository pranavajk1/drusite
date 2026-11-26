-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Drusite's core schema: accounts, workspaces, members and workspace keys.
--
-- Row-level security is the second line between workspaces, behind the Cedar policy: every
-- workspace-scoped table is FORCEd, so its policies apply even to the role that owns it, and
-- an application bug that forgets a filter still can't read another workspace's rows. The
-- server sets `drusite.user_id` at the start of each transaction it runs for a user; jobs that
-- serve no single user (compaction, key rotation) set `drusite.system`. Both are
-- transaction-local. IDs are UUIDv7s made by the server, never by Postgres.

DO $$
BEGIN
    IF current_setting('server_version_num')::int < 160000 THEN
        RAISE EXCEPTION 'Drusite needs PostgreSQL 16 or newer, this is %', version();
    END IF;
END
$$;

-- pgvector isn't a trusted extension, so a database administrator creates it once; the
-- server checks for it before migrating and says how.
CREATE EXTENSION IF NOT EXISTS vector;

CREATE FUNCTION drusite_user_id() RETURNS uuid
    LANGUAGE sql STABLE PARALLEL SAFE
    AS $$ SELECT nullif(current_setting('drusite.user_id', true), '')::uuid $$;

CREATE FUNCTION drusite_is_system() RETURNS boolean
    LANGUAGE sql STABLE PARALLEL SAFE
    AS $$ SELECT coalesce(current_setting('drusite.system', true), '') = 'on' $$;

-- Accounts. Not workspace data, so no row-level security: sign-in looks accounts up before
-- anyone is known.
CREATE TABLE users (
    id uuid PRIMARY KEY,
    email text NOT NULL CHECK (email <> ''),
    email_verified boolean NOT NULL DEFAULT false,
    display_name text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    disabled_at timestamptz
);
CREATE UNIQUE INDEX users_email_key ON users (lower(email));

-- One-time links made by `drusite-server account recover` (and `account create`). Only a
-- hash of each token is kept.
CREATE TABLE recovery_tokens (
    token_hash bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at timestamptz
);
CREATE INDEX recovery_tokens_user ON recovery_tokens (user_id);

CREATE TABLE workspaces (
    id uuid PRIMARY KEY,
    created_by uuid NOT NULL REFERENCES users,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE workspace_members (
    workspace_id uuid NOT NULL REFERENCES workspaces ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    role text NOT NULL CHECK (role IN ('owner', 'editor', 'viewer')),
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, user_id)
);
CREATE INDEX workspace_members_user ON workspace_members (user_id);

-- The current user's role in a workspace, or NULL. Reads only the user's own membership row,
-- which the policy on workspace_members always allows, so policies can call it without
-- recursing. PL/pgSQL, so the planner can't inline it into workspace_members' own policies
-- (Postgres rejects a policy whose expansion refers back to its table).
CREATE FUNCTION drusite_role(ws uuid) RETURNS text
    LANGUAGE plpgsql STABLE PARALLEL SAFE
    AS $$
    BEGIN
        RETURN (
            SELECT role FROM workspace_members
            WHERE workspace_id = ws AND user_id = drusite_user_id()
        );
    END
    $$;

-- Each workspace's data key, wrapped by a master key from the key source.
CREATE TABLE workspace_keys (
    workspace_id uuid NOT NULL REFERENCES workspaces ON DELETE CASCADE,
    version integer NOT NULL CHECK (version > 0),
    master_key_id text NOT NULL,
    wrapped bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, version)
);

ALTER TABLE workspaces ENABLE ROW LEVEL SECURITY;
ALTER TABLE workspaces FORCE ROW LEVEL SECURITY;
ALTER TABLE workspace_members ENABLE ROW LEVEL SECURITY;
ALTER TABLE workspace_members FORCE ROW LEVEL SECURITY;
ALTER TABLE workspace_keys ENABLE ROW LEVEL SECURITY;
ALTER TABLE workspace_keys FORCE ROW LEVEL SECURITY;

-- Workspaces: members see them; anyone may create one as themselves; owners change or delete.
CREATE POLICY workspaces_select ON workspaces FOR SELECT
    USING (drusite_is_system() OR created_by = drusite_user_id() OR drusite_role(id) IS NOT NULL);
CREATE POLICY workspaces_insert ON workspaces FOR INSERT
    WITH CHECK (drusite_is_system() OR created_by = drusite_user_id());
CREATE POLICY workspaces_update ON workspaces FOR UPDATE
    USING (drusite_is_system() OR drusite_role(id) = 'owner');
CREATE POLICY workspaces_delete ON workspaces FOR DELETE
    USING (drusite_is_system() OR drusite_role(id) = 'owner');

-- Members: each user sees their own memberships (a co-member list comes with sharing, through
-- the system context). Owners add, change and remove members; whoever created a workspace may
-- make themselves its first owner.
CREATE POLICY members_select ON workspace_members FOR SELECT
    USING (drusite_is_system() OR user_id = drusite_user_id());
CREATE POLICY members_insert ON workspace_members FOR INSERT
    WITH CHECK (
        drusite_is_system()
        OR drusite_role(workspace_id) = 'owner'
        OR (
            user_id = drusite_user_id()
            AND role = 'owner'
            AND EXISTS (
                SELECT 1 FROM workspaces w
                WHERE w.id = workspace_id AND w.created_by = drusite_user_id()
            )
        )
    );
CREATE POLICY members_update ON workspace_members FOR UPDATE
    USING (drusite_is_system() OR drusite_role(workspace_id) = 'owner');
CREATE POLICY members_delete ON workspace_members FOR DELETE
    USING (drusite_is_system() OR drusite_role(workspace_id) = 'owner');

-- Keys: members read them (the server needs the key to serve them); the first key is made by
-- the owner when the workspace is created; rotation runs as the system.
CREATE POLICY keys_select ON workspace_keys FOR SELECT
    USING (drusite_is_system() OR drusite_role(workspace_id) IS NOT NULL);
CREATE POLICY keys_insert ON workspace_keys FOR INSERT
    WITH CHECK (drusite_is_system() OR drusite_role(workspace_id) = 'owner');
CREATE POLICY keys_update ON workspace_keys FOR UPDATE
    USING (drusite_is_system());
CREATE POLICY keys_delete ON workspace_keys FOR DELETE
    USING (drusite_is_system());
