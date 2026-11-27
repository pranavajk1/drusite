-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Search: full-text and semantic, over the live documents.
--
-- Nothing here is the notes' text. Each block has its tsvector (the words, which is what full-text
-- search needs) and, when an embedding model is configured, its embedding; snippets are made by
-- opening the sealed document when someone searches. Both still reveal what a block is about to
-- anyone who can read the database or its backups: they are derived from the plain text. A
-- deployment that can't accept that turns server search off (`[search] enabled = false`).

-- Documents waiting to be indexed. A trigger adds one whenever its log grows; the indexer takes
-- them a few seconds later, so a burst of typing is indexed once.
CREATE TABLE search_queue (
    doc_id uuid PRIMARY KEY REFERENCES documents ON DELETE CASCADE,
    workspace_id uuid NOT NULL,
    queued_at timestamptz NOT NULL DEFAULT now(),
    attempts int NOT NULL DEFAULT 0
);
CREATE INDEX search_queue_due ON search_queue (queued_at);

-- One row per block, plus one per page for its title (block_id is the nil UUID).
CREATE TABLE search_blocks (
    workspace_id uuid NOT NULL REFERENCES workspaces ON DELETE CASCADE,
    page_id uuid NOT NULL,
    block_id uuid NOT NULL,
    -- The text's BLAKE3 hash, keyed by a key derived from the workspace's, so unchanged text isn't
    -- embedded again, and the hash can't be used to test a guess at the text.
    content_hash bytea NOT NULL,
    -- Which text search configuration made `tsv` ('simple' unless configured).
    config regconfig NOT NULL,
    tsv tsvector NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, page_id, block_id)
);
CREATE INDEX search_blocks_tsv ON search_blocks USING gin (tsv);

-- Embeddings, one per block and model. Vectors from different models can't be compared, so
-- every search filters on the model; an approximate-nearest-neighbour index is made per model
-- once its dimensions are known (see the server's search module).
CREATE TABLE search_embeddings (
    workspace_id uuid NOT NULL REFERENCES workspaces ON DELETE CASCADE,
    page_id uuid NOT NULL,
    block_id uuid NOT NULL,
    model text NOT NULL,
    content_hash bytea NOT NULL,
    embedding vector NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, page_id, block_id, model)
);

ALTER TABLE search_blocks ENABLE ROW LEVEL SECURITY;
ALTER TABLE search_blocks FORCE ROW LEVEL SECURITY;
ALTER TABLE search_embeddings ENABLE ROW LEVEL SECURITY;
ALTER TABLE search_embeddings FORCE ROW LEVEL SECURITY;

-- search_queue has no row-level security: it holds only IDs, and the trigger writes it in the
-- writer's own transaction, including the one that claims a new workspace, where the documents
-- are inserted before the owner's membership exists.

-- Members search their workspaces; only the indexer writes.
CREATE POLICY search_blocks_select ON search_blocks FOR SELECT
    USING (drusite_is_system() OR drusite_role(workspace_id) IS NOT NULL);
CREATE POLICY search_blocks_write ON search_blocks FOR ALL
    USING (drusite_is_system());
CREATE POLICY search_embeddings_select ON search_embeddings FOR SELECT
    USING (drusite_is_system() OR drusite_role(workspace_id) IS NOT NULL);
CREATE POLICY search_embeddings_write ON search_embeddings FOR ALL
    USING (drusite_is_system());

CREATE FUNCTION drusite_search_enqueue() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    doc uuid;
BEGIN
    -- Only the field the row has may be named: plpgsql fails on the other one.
    IF TG_TABLE_NAME = 'documents' THEN
        doc := NEW.id;
    ELSE
        doc := NEW.doc_id;
    END IF;
    INSERT INTO search_queue (doc_id, workspace_id)
    VALUES (doc, NEW.workspace_id)
    ON CONFLICT (doc_id) DO UPDATE SET queued_at = now(), attempts = 0;
    RETURN NULL;
END;
$$;

CREATE TRIGGER documents_search AFTER INSERT ON documents
    FOR EACH ROW EXECUTE FUNCTION drusite_search_enqueue();
CREATE TRIGGER document_updates_search AFTER INSERT ON document_updates
    FOR EACH ROW EXECUTE FUNCTION drusite_search_enqueue();
