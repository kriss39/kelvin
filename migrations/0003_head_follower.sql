-- The head follower starts near the head and moves forward; the backfill fills in everything below
-- the follower's starting height and then stops.
ALTER TABLE ingest_state
    ADD COLUMN tip_from       BIGINT,
    ADD COLUMN tip_next       BIGINT,
    ADD COLUMN tip_updated_at TIMESTAMPTZ;

-- Noteworthy things the indexer saw, e.g. kind 'reset_suspected' when a stored block no longer
-- matches what the RPC serves at that height.
CREATE TABLE events (
    network SMALLINT    NOT NULL,
    ts      TIMESTAMPTZ NOT NULL DEFAULT now(),
    kind    TEXT        NOT NULL,
    detail  TEXT        NOT NULL
);
CREATE INDEX events_recent ON events (network, ts DESC);
