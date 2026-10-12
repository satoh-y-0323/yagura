CREATE TABLE plan (
    plan_id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    cwd TEXT NOT NULL,
    owner_window TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE plan_task (
    plan_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    title TEXT NOT NULL,
    role TEXT,
    parent_profile TEXT,
    parent_model TEXT,
    parent_effort TEXT,
    user_profile TEXT,
    user_model TEXT,
    user_effort TEXT,
    window_id TEXT,
    PRIMARY KEY (plan_id, task_id)
);
CREATE TABLE plan_task_dep (
    plan_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    depends_on TEXT NOT NULL,
    PRIMARY KEY (plan_id, task_id, depends_on)
);
CREATE TABLE plan_task_write (
    plan_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (plan_id, task_id, path)
);
