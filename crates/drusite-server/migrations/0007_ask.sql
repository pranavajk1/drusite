-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Ask your notes: saved conversations, and the record of what was sent to which AI provider.

-- A person's conversations with their notes, one workspace at a time. Only they see them, not
-- the workspace's other members: a question says as much about the asker as the answer does
-- about the notes. Titles and messages are sealed with the workspace's key, like the documents.
CREATE TABLE ask_conversations (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    title bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ask_conversations_recent ON ask_conversations (user_id, workspace_id, updated_at DESC);

CREATE TABLE ask_messages (
    id uuid PRIMARY KEY,
    conversation_id uuid NOT NULL REFERENCES ask_conversations ON DELETE CASCADE,
    workspace_id uuid NOT NULL REFERENCES workspaces ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users ON DELETE CASCADE,
    role text NOT NULL CHECK (role IN ('user', 'assistant')),
    content bytea NOT NULL,
    -- The blocks the answer drew on, numbered as it cites them: [{"n", "page", "block"}]. IDs
    -- only; titles and snippets are looked up when the conversation is opened.
    sources jsonb NOT NULL DEFAULT '[]',
    -- Which model answered, and why it stopped short if it did.
    model text,
    error text,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ask_messages_order ON ask_messages (conversation_id, created_at, id);

ALTER TABLE ask_conversations ENABLE ROW LEVEL SECURITY;
ALTER TABLE ask_conversations FORCE ROW LEVEL SECURITY;
ALTER TABLE ask_messages ENABLE ROW LEVEL SECURITY;
ALTER TABLE ask_messages FORCE ROW LEVEL SECURITY;

-- Theirs, in a workspace they still belong to.
CREATE POLICY ask_conversations_own ON ask_conversations FOR ALL
    USING (drusite_is_system()
           OR (user_id = drusite_user_id() AND drusite_role(workspace_id) IS NOT NULL))
    WITH CHECK (drusite_is_system()
                OR (user_id = drusite_user_id() AND drusite_role(workspace_id) IS NOT NULL));
CREATE POLICY ask_messages_own ON ask_messages FOR ALL
    USING (drusite_is_system()
           OR (user_id = drusite_user_id() AND drusite_role(workspace_id) IS NOT NULL))
    WITH CHECK (drusite_is_system()
                OR (user_id = drusite_user_id() AND drusite_role(workspace_id) IS NOT NULL));

-- Every call to an AI provider that carried something from the notes: which feature, which
-- provider and model, at which host, which blocks (IDs only), how much text and how many
-- tokens, and how it went. Never the text itself. Also what the per-person limits count.
CREATE TABLE ai_audit (
    id uuid PRIMARY KEY,
    -- None for a search over all of a person's workspaces.
    workspace_id uuid REFERENCES workspaces ON DELETE CASCADE,
    -- None for the indexer, which works for nobody in particular.
    user_id uuid REFERENCES users ON DELETE SET NULL,
    -- 'ask' (an answer), 'ask-embed' (a question's vector), 'search-embed' (a search's
    -- vector), 'index' (blocks' vectors).
    feature text NOT NULL,
    provider text NOT NULL,
    host text NOT NULL,
    model text NOT NULL,
    -- [[page, block], …] for an answer; the page for indexing.
    blocks jsonb NOT NULL DEFAULT '[]',
    items int NOT NULL DEFAULT 0,
    chars int NOT NULL DEFAULT 0,
    input_tokens int,
    output_tokens int,
    -- 'pending' while it runs, then 'ok' or what went wrong, as a category.
    outcome text NOT NULL DEFAULT 'pending',
    at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ai_audit_recent ON ai_audit (workspace_id, at DESC);
CREATE INDEX ai_audit_limits ON ai_audit (user_id, feature, at DESC);

ALTER TABLE ai_audit ENABLE ROW LEVEL SECURITY;
ALTER TABLE ai_audit FORCE ROW LEVEL SECURITY;

-- A person sees what was sent for them; a workspace's owner sees everything sent from it.
CREATE POLICY ai_audit_select ON ai_audit FOR SELECT
    USING (drusite_is_system()
           OR (user_id = drusite_user_id()
               AND (workspace_id IS NULL OR drusite_role(workspace_id) IS NOT NULL))
           OR drusite_role(workspace_id) = 'owner');
CREATE POLICY ai_audit_insert ON ai_audit FOR INSERT
    WITH CHECK (drusite_is_system()
                OR (user_id = drusite_user_id()
                    AND (workspace_id IS NULL OR drusite_role(workspace_id) IS NOT NULL)));
CREATE POLICY ai_audit_update ON ai_audit FOR UPDATE
    USING (drusite_is_system() OR user_id = drusite_user_id());
CREATE POLICY ai_audit_delete ON ai_audit FOR DELETE
    USING (drusite_is_system());
