-- # Queries 
-- * List of intent IDs that can be included in a new bundle (no inclusion + underpriced inclusion)
--   > Query all rows with nonce > finalized nonce for sender address
-- * For a specific historic intent ID, retrieve which transaction and block is included into efficiently
--   > Query rows WHERE id = ? and pick the most recent or canonical
-- * Prune inclusions for dropped transactions
--   > Remove rows WHERE tx_hash = ?
--
-- # Pruning
-- Prunes non canonical re-priced transactions 
--
-- # Mutation
-- None
-- 
CREATE TABLE intent_inclusions (
    -- data intent UUID as binary
    id BINARY(16) NOT NULL,
    -- Transaction hash that includes this set of IDs
    tx_hash BINARY(32) NOT NULL,
    -- Tx from
    sender_address BINARY(20) NOT NULL,
    -- Tx nonce from sender
    nonce INT NOT NULL,
    -- Timestamp with milisecond level precision, automatically populated
    updated_at TIMESTAMP(3) DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),

    PRIMARY KEY(id, tx_hash),
    -- Allow to query inclusions by data intent ID or transaction hash
    INDEX(id),
    INDEX(tx_hash),
    INDEX(sender_address),
    INDEX(nonce)
);
