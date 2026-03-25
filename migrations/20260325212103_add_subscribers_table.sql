CREATE TABLE media_request_subscribers (
    request_id INTEGER NOT NULL REFERENCES media_requests(id) ON DELETE CASCADE,
    username VARCHAR(255) NOT NULL,
    PRIMARY KEY (request_id, username)
);
INSERT INTO media_request_subscribers (request_id, username)
SELECT id, username FROM media_requests;
ALTER TABLE media_requests DROP COLUMN username CASCADE;