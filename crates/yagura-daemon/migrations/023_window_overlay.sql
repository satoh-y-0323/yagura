-- Whether a window's launch overlay was made, as its open or its overlay
-- door settled it, for a window whose profile declares one. A row without
-- one was written before the fact was kept.
ALTER TABLE windows ADD COLUMN overlay TEXT;
