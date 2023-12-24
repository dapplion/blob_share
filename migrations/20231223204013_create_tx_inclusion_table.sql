CREATE TABLE blob_txs (
    -- Transaction hash as binary
    tx_hash BINARY(32) PRIMARY KEY,
    -- tx.from as binary
    sender BINARY(20)
)
