CREATE TABLE default_palettes (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    palettes_json TEXT NOT NULL
);
