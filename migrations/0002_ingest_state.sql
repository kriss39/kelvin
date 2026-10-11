-- Backfill cursor: it walks from the oldest retained block towards the head, then follows the head.
CREATE TABLE ingest_state (
    network       SMALLINT    PRIMARY KEY,
    -- oldest block the RPC served when the backfill started
    backfill_from BIGINT      NOT NULL,
    -- every height below this has been indexed or recorded in missing_blocks
    backfill_next BIGINT      NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Heights that could not be indexed. reason: 'not_listed' (absent from getBlocks), 'not_found'
-- (listed, but getBlock said not found), 'pruned' (gone before the backfill reached them) or
-- 'invalid: <error>' (the block did not normalize).
CREATE TABLE missing_blocks (
    network     SMALLINT    NOT NULL,
    from_height BIGINT      NOT NULL,
    to_height   BIGINT      NOT NULL,
    reason      TEXT        NOT NULL,
    seen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (network, from_height)
);

-- Oldest block the RPC serves, sampled hourly: shows whether pruning is a sliding window.
CREATE TABLE retention (
    network SMALLINT    NOT NULL,
    ts      TIMESTAMPTZ NOT NULL DEFAULT now(),
    oldest  BIGINT      NOT NULL,
    head    BIGINT      NOT NULL,
    PRIMARY KEY (network, ts)
);
