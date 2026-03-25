ALTER TABLE media_requests DROP COLUMN requested_seasons;
ALTER TABLE media_requests ADD COLUMN requested_season INTEGER;
