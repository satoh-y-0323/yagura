-- Parcels the daemon no longer keeps: a handoff envelope and the report
-- notice live in memory alone, so the rows an older daemon kept for them are
-- dropped. Moving another kind of parcel to unsaved adds its rows here, in
-- the same change.
DELETE FROM inbox_parcels WHERE key LIKE 'handoff:%';
DELETE FROM inbox_parcels WHERE key = 'report' AND notice = 1;
