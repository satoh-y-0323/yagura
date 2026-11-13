-- The `[hook_timeout]` table of the profile a window was opened from, as it
-- was then, for a window opened from a profile. A row without one was written
-- before the fact was kept, or is of a window opened from no profile.
ALTER TABLE windows ADD COLUMN hook_timeouts TEXT;
