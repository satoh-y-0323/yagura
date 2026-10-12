ALTER TABLE chat_room ADD COLUMN workspace_kind TEXT;
ALTER TABLE chat_room ADD COLUMN workspace_key TEXT;
ALTER TABLE chat_room ADD COLUMN created_at_ms INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX chat_room_workspace ON chat_room (workspace_kind, workspace_key) WHERE workspace_key IS NOT NULL;
