CREATE TABLE post_data_nonces (
    -- 0x prefixed hex encoded lowercase address
    eth_address CHAR(42) PRIMARY KEY,
    -- BIGINT = 64 bits signed
    nonce BIGINT NOT NULL
);

