-- Add migration script here
CREATE TABLE IF NOT EXISTS steam_ids (
	steamid64 BIGINT PRIMARY KEY,
	timecreated BIGINT NOT NULL
);