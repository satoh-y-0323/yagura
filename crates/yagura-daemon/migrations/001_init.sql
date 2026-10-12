CREATE TABLE snapshot_meta (
    id                    INTEGER PRIMARY KEY CHECK (id = 1),
    taken_at_epoch_s      INTEGER NOT NULL,
    prev_taken_at_epoch_s INTEGER
);
CREATE TABLE snapshot_windows (
    slot           INTEGER NOT NULL CHECK (slot IN (0, 1)),
    position       INTEGER NOT NULL,
    command_json   TEXT NOT NULL,
    cwd            TEXT NOT NULL,
    name           TEXT NOT NULL,
    vendor         TEXT,
    detection      TEXT,
    PRIMARY KEY (slot, position)
);
