CREATE TABLE chat_room (
    room_id TEXT PRIMARY KEY,
    halted_reason TEXT,
    halted_at_ms INTEGER,
    needs_user INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE chat_message (
    room_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    at_ms INTEGER NOT NULL,
    from_kind TEXT NOT NULL,
    from_window_id TEXT,
    from_name TEXT NOT NULL,
    body TEXT NOT NULL,
    mentions_json TEXT NOT NULL,
    broadcast INTEGER NOT NULL DEFAULT 0,
    silence_noted INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (room_id, seq)
);
CREATE TABLE chat_delivery (
    room_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    window_id TEXT NOT NULL,
    state TEXT NOT NULL,
    at_ms INTEGER NOT NULL,
    PRIMARY KEY (room_id, seq, window_id)
);
INSERT INTO chat_room (room_id) VALUES ('main');
