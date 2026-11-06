CREATE TABLE windows (
    opened_seq INTEGER PRIMARY KEY AUTOINCREMENT,
    window_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    cwd TEXT NOT NULL,
    vendor TEXT,
    command_json TEXT NOT NULL,
    detection TEXT,
    profile TEXT,
    model TEXT,
    effort TEXT,
    layout_json TEXT,
    session_id TEXT,
    opener TEXT,
    task_plan_id TEXT,
    task_id TEXT,
    credentials TEXT NOT NULL CHECK (credentials IN ('env', 'launch')),
    state TEXT NOT NULL CHECK (state IN ('opening', 'live', 'lost'))
);
