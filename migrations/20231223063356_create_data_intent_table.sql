CREATE TABLE data_intents (
    -- 0x prefixed hex encoded lowercase address
    eth_address CHAR(42) PRIMARY KEY,
    -- binary data to publish, MEDIUMBLOB = binary large object with max length of 2^24-1 bytes (16MB)
    data MEDIUMBLOB NOT NULL,
    -- non-prefixed hex encoded lowercase hash of data
    data_hash CHAR(64) NOT NULL,
    -- Max BIGINT = 2^63-1. Max gas price possible to represent is 9.2 ETH / byte, or 1,208,925 ETH per blob
    max_blob_gas_price BIGINT NOT NULL,
    -- Optional ECDSA signature over data_hash, serialized
    data_hash_signature BINARY(65) DEFAULT NULL,
    -- Timestamp with milisecond level precision, automatically populated
    created_at TIMESTAMP(3) DEFAULT CURRENT_TIMESTAMP(3),
    -- Index created_at to query recently added intents efficiently. The query:
    -- `EXPLAIN ANALYZE SELECT * FROM data_intents WHERE created_at > '2024-01-01 00:00:00';`
    -- against PlanetScale consumes 0 row read credits if there are no matches.
    INDEX(created_at)
);
