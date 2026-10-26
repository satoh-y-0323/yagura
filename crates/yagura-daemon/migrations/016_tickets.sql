CREATE TABLE ticket_workspace (
    workspace_id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('git', 'directory')),
    key TEXT NOT NULL,
    next_number INTEGER NOT NULL,
    UNIQUE (kind, key)
);
CREATE TABLE ticket (
    workspace_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    state TEXT NOT NULL,
    milestone TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, number)
);
CREATE TABLE ticket_field (
    workspace_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (workspace_id, number, name)
);
CREATE TABLE ticket_result (
    workspace_id INTEGER NOT NULL,
    number INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    at INTEGER NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY (workspace_id, number, seq)
);
CREATE TABLE ticket_milestone (
    workspace_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    position INTEGER NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    PRIMARY KEY (workspace_id, name)
);
ALTER TABLE plan ADD COLUMN ticket_workspace_id INTEGER;
ALTER TABLE plan ADD COLUMN ticket_number INTEGER;
