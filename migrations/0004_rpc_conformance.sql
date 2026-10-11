-- Hourly checks of documented RPC quirks. `issue` is true while the quirk is present, so a change
-- in the RPC's behaviour shows up as a flip; `observed` records what was seen.
CREATE TABLE rpc_conformance (
    network  SMALLINT    NOT NULL,
    ts       TIMESTAMPTZ NOT NULL DEFAULT now(),
    check_id TEXT        NOT NULL,
    issue    BOOLEAN     NOT NULL,
    observed TEXT        NOT NULL,
    PRIMARY KEY (network, check_id, ts)
);
