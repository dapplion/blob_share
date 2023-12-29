-- Catch all table for user related data
CREATE TABLE users (
    -- 0x prefixed hex encoded lowercase address
    eth_address CHAR(42) PRIMARY KEY,
    -- BIGINT = 64 bits signed, optional only required through an unsafe channel for replay protection
    post_data_nonce BIGINT 
);
