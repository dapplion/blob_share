CREATE TABLE anchor_block (
    -- AnchorBlock serialized as JSON
    -- TODO: Serialize as compact binary format
    anchor_block_json LONGTEXT NOT NULL, 
    -- Block number of the AnchorBlock serialized value
    block_number INT UNSIGNED NOT NULL
);
