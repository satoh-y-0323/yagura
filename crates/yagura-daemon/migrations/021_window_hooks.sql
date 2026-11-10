-- The route a window's hooks take, as its open settled it. A row without one
-- (written before the spawn settled it) reads as hooks that do not reach the
-- window's CLI; the daemon alone decides that reading.
ALTER TABLE windows ADD COLUMN hooks TEXT;
