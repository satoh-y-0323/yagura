-- Whether a window's CLI runs in another environment (a WSL guest), as its
-- open settled it. A row without one was written before the fact was kept.
ALTER TABLE windows ADD COLUMN guest INTEGER;
