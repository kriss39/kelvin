-- Every address gets a BIGINT id in `addresses`; other tables store the 8-byte id instead of the
-- 32-byte key (about a third less space per indexed row). Signatures and instruction data are raw
-- bytes (decoded base58).
-- A transaction is identified by (network, height, idx); child tables reference that, not the signature.
-- Columns are ordered 8-byte, 4-byte, 2-byte, then variable-length, so rows carry no alignment padding.

CREATE TABLE networks (
    id             SMALLINT PRIMARY KEY,
    name           TEXT NOT NULL UNIQUE,
    rpc_url        TEXT NOT NULL,
    genesis_marker BYTEA,
    epoch_seen     INT NOT NULL DEFAULT 0
);

INSERT INTO networks (id, name, rpc_url) VALUES
    (1, 'testnet', 'https://testnet.rialo.io'),
    (2, 'devnet',  'https://devnet.rialo.io');

-- Shared by all networks: a key means the same account everywhere.
CREATE TABLE addresses (
    id      BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    address BYTEA  NOT NULL UNIQUE
);

CREATE TABLE blocks (
    height       BIGINT   NOT NULL,
    block_time_s BIGINT   NOT NULL,
    -- First 8 bytes of sha256 over the block's signatures (Rialo exposes no block hash); used
    -- to notice when a network has been reset and a height now holds a different block.
    fingerprint  BIGINT   NOT NULL,
    tx_count     INT      NOT NULL,
    sys_tx_count INT      NOT NULL,
    network      SMALLINT NOT NULL,
    PRIMARY KEY (network, height)
);

-- User transactions only. System transactions (every instruction is TokenomicsGovernance)
-- are counted in blocks.sys_tx_count and not stored.
CREATE TABLE txs (
    height          BIGINT   NOT NULL,
    fee             BIGINT   NOT NULL,
    cu              BIGINT,
    valid_from_ms   BIGINT,
    fee_payer       BIGINT   NOT NULL,
    idx             INT      NOT NULL,
    network         SMALLINT NOT NULL,
    num_signers     SMALLINT NOT NULL,
    num_ro_signed   SMALLINT NOT NULL,
    num_ro_unsigned SMALLINT NOT NULL,
    signature       BYTEA    NOT NULL,
    err             JSONB,
    -- message.accountKeys as address ids, in message order
    account_keys    BIGINT[] NOT NULL,
    PRIMARY KEY (network, height, idx)
);
CREATE UNIQUE INDEX txs_signature ON txs (network, signature);

-- Main index for address history. role: 0 signer+writable, 1 signer+readonly, 2 writable, 3 readonly.
-- Programs invoked by the transaction are left out (System alone would be in every row); program
-- history comes from instructions_program instead.
CREATE TABLE tx_accounts (
    height  BIGINT   NOT NULL,
    address BIGINT   NOT NULL,
    idx     INT      NOT NULL,
    network SMALLINT NOT NULL,
    role    SMALLINT NOT NULL,
    PRIMARY KEY (network, address, height, idx)
);

-- inner_idx = -1 for top-level instructions, otherwise the position among the inner instructions
-- of top-level instruction ix_idx.
CREATE TABLE instructions (
    height    BIGINT   NOT NULL,
    program   BIGINT   NOT NULL,
    idx       INT      NOT NULL,
    network   SMALLINT NOT NULL,
    ix_idx    SMALLINT NOT NULL,
    inner_idx SMALLINT NOT NULL,
    kind      TEXT,
    data      BYTEA    NOT NULL,
    -- one byte per account, an index into txs.account_keys
    accounts  BYTEA    NOT NULL,
    PRIMARY KEY (network, height, idx, ix_idx, inner_idx)
);
CREATE INDEX instructions_program ON instructions (network, program, height DESC, idx DESC);

-- Value movements decoded from successful transactions. mint NULL = native RLO.
CREATE TABLE transfers (
    height    BIGINT   NOT NULL,
    source    BIGINT   NOT NULL,
    dest      BIGINT   NOT NULL,
    mint      BIGINT,
    idx       INT      NOT NULL,
    network   SMALLINT NOT NULL,
    ix_idx    SMALLINT NOT NULL,
    inner_idx SMALLINT NOT NULL,
    amount    NUMERIC(20, 0) NOT NULL,
    PRIMARY KEY (network, height, idx, ix_idx, inner_idx)
);
CREATE INDEX transfers_source ON transfers (network, source, height DESC, idx DESC);
CREATE INDEX transfers_dest   ON transfers (network, dest, height DESC, idx DESC);

CREATE TABLE logs (
    height  BIGINT   NOT NULL,
    idx     INT      NOT NULL,
    network SMALLINT NOT NULL,
    -- logMessages joined with '\n'
    lines   TEXT     NOT NULL,
    PRIMARY KEY (network, height, idx)
);
