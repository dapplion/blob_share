CREATE TABLE users (
    -- 0x prefixed hex encoded lowercase address
    eth_address CHAR(42) PRIMARY KEY,
    -- Store numbers with no decimal precision up to 1e38 ~ 2**128
    -- Cache of total data intent costs to prevent having to read all data intent rows
    total_data_intent_cost DECIMAL(38, 0) NOT NULL,
    -- BIGINT = 64 bits signed, optional only required through an unsafe channel for replay protection
    post_data_nonce BIGINT 
);
