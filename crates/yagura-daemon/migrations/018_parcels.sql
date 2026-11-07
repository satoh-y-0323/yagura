CREATE TABLE inbox_parcels (
    window_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    key TEXT NOT NULL,
    notice INTEGER NOT NULL CHECK (notice IN (0, 1)),
    body TEXT NOT NULL,
    PRIMARY KEY (window_id, seq)
);
CREATE TABLE report_parcels (
    window_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    report_json TEXT NOT NULL,
    PRIMARY KEY (window_id, seq)
);
CREATE TRIGGER windows_take_their_parcels AFTER DELETE ON windows
BEGIN
    DELETE FROM inbox_parcels WHERE window_id = OLD.window_id;
    DELETE FROM report_parcels WHERE window_id = OLD.window_id;
END;
