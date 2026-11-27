-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Attachments: files on pages, stored sealed in the object store.
--
-- A page refers to an attachment by the BLAKE3 hash of its content, which devices can compute.
-- That hash isn't kept here: each row is found by `lookup`, the hash keyed with the workspace's
-- key, so neither the database nor the bucket alone can confirm that a workspace holds a file
-- someone already has.
CREATE TABLE attachments (
    workspace_id uuid NOT NULL REFERENCES workspaces ON DELETE CASCADE,
    -- The content hash keyed with key version `key_version`.
    lookup text NOT NULL,
    key_version int NOT NULL,
    -- Its address in the object store (see the server's storage module).
    address text NOT NULL,
    size bigint NOT NULL,
    created_by uuid REFERENCES users ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, lookup)
);

ALTER TABLE attachments ENABLE ROW LEVEL SECURITY;
ALTER TABLE attachments FORCE ROW LEVEL SECURITY;

CREATE POLICY attachments_select ON attachments FOR SELECT
    USING (drusite_is_system() OR drusite_role(workspace_id) IS NOT NULL);
CREATE POLICY attachments_insert ON attachments FOR INSERT
    WITH CHECK (
        drusite_is_system()
        OR (created_by = drusite_user_id() AND drusite_role(workspace_id) IN ('owner', 'editor'))
    );
CREATE POLICY attachments_delete ON attachments FOR DELETE
    USING (drusite_is_system() OR drusite_role(workspace_id) = 'owner');
