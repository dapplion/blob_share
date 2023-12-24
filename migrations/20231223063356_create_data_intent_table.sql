CREATE TABLE data_intents (
    id BINARY(16) PRIMARY KEY, -- UUID as binary
    -- sender address
    eth_address BINARY(20),
    -- binary data to publish, MEDIUMBLOB = binary large object with max length of 2^24-1 bytes (16MB)
    data MEDIUMBLOB NOT NULL,
    -- byte length of data, max possible size is 131,072 < INT::MAX = 2,147,483,647
    data_len INT UNSIGNED NOT NULL,
    -- hash of data (keccak256)
    data_hash BINARY(32) NOT NULL,
    -- Max BIGINT = 2^63-1. Max gas price possible to represent is 9.2 ETH / byte, or 1,208,925 ETH per blob
    max_blob_gas_price BIGINT UNSIGNED NOT NULL,
    -- Optional ECDSA signature over data_hash, serialized
    data_hash_signature BINARY(65) DEFAULT NULL,
    -- Transaction hash of the included 
    inclusion_tx_hash BINARY(32) DEFAULT NULL,
    -- Timestamp with milisecond level precision, automatically populated
    updated_at TIMESTAMP(3) DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
    -- Index created_at to query recently added intents efficiently. The query:
    -- `EXPLAIN ANALYZE SELECT * FROM data_intents WHERE updated_at > '2024-01-01 00:00:00';`
    -- against PlanetScale consumes 0 row read credits if there are no matches.
    INDEX(updated_at)
);
