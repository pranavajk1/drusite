-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Sync: every workspace document and page document, as a log of Loro updates plus a compacted
-- snapshot. Entries are sealed with the workspace's key and bound to their document and
-- sequence number, so the database alone reads nothing and can't move an entry elsewhere.

CREATE TABLE documents (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('workspace', 'page')),
    -- The newest sequence number in the log. Each push takes the next one, with this row locked.
    head_seq bigint NOT NULL DEFAULT 0,
    -- The snapshot covers every entry up to here; older updates are gone.
    snapshot_seq bigint NOT NULL DEFAULT 0,
    -- Every version the log holds (an encoded Loro version vector). A push that starts beyond
    -- it would leave a hole in the log, so it's refused, and devices are told this on every
    -- subscription so they push exactly what's missing.
    known_version bytea NOT NULL DEFAULT '',
    created_by uuid NOT NULL REFERENCES users,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    -- The workspace document's ID is the workspace's own.
    CHECK (kind <> 'workspace' OR id = workspace_id)
);
CREATE INDEX documents_workspace ON documents (workspace_id);

CREATE TABLE document_updates (
    doc_id uuid NOT NULL REFERENCES documents ON DELETE CASCADE,
    workspace_id uuid NOT NULL,
    seq bigint NOT NULL CHECK (seq > 0),
    data bytea NOT NULL,
    -- The connection that pushed it, so it isn't sent back to that connection.
    origin uuid,
    created_by uuid REFERENCES users ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (doc_id, seq)
);

CREATE TABLE document_snapshots (
    doc_id uuid PRIMARY KEY REFERENCES documents ON DELETE CASCADE,
    workspace_id uuid NOT NULL,
    through_seq bigint NOT NULL,
    data bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE documents ENABLE ROW LEVEL SECURITY;
ALTER TABLE documents FORCE ROW LEVEL SECURITY;
ALTER TABLE document_updates ENABLE ROW LEVEL SECURITY;
ALTER TABLE document_updates FORCE ROW LEVEL SECURITY;
ALTER TABLE document_snapshots ENABLE ROW LEVEL SECURITY;
ALTER TABLE document_snapshots FORCE ROW LEVEL SECURITY;

-- Members read; editors and owners write. Compaction rewrites snapshots and drops old updates
-- as the system. Behind Cedar, which decides first.
CREATE POLICY documents_select ON documents FOR SELECT
    USING (drusite_is_system() OR drusite_role(workspace_id) IS NOT NULL);
CREATE POLICY documents_insert ON documents FOR INSERT
    WITH CHECK (
        drusite_is_system()
        OR (created_by = drusite_user_id() AND drusite_role(workspace_id) IN ('owner', 'editor'))
    );
CREATE POLICY documents_update ON documents FOR UPDATE
    USING (drusite_is_system() OR drusite_role(workspace_id) IN ('owner', 'editor'));
CREATE POLICY documents_delete ON documents FOR DELETE
    USING (drusite_is_system() OR drusite_role(workspace_id) = 'owner');

CREATE POLICY updates_select ON document_updates FOR SELECT
    USING (drusite_is_system() OR drusite_role(workspace_id) IS NOT NULL);
CREATE POLICY updates_insert ON document_updates FOR INSERT
    WITH CHECK (
        drusite_is_system()
        OR (
            drusite_role(workspace_id) IN ('owner', 'editor')
            AND EXISTS (
                SELECT 1 FROM documents d
                WHERE d.id = doc_id AND d.workspace_id = document_updates.workspace_id
            )
        )
    );
CREATE POLICY updates_delete ON document_updates FOR DELETE
    USING (drusite_is_system());

CREATE POLICY snapshots_select ON document_snapshots FOR SELECT
    USING (drusite_is_system() OR drusite_role(workspace_id) IS NOT NULL);
CREATE POLICY snapshots_write ON document_snapshots FOR ALL
    USING (drusite_is_system())
    WITH CHECK (drusite_is_system());
