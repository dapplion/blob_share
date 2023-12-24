CREATE TABLE intents_tx_inclusion (
    -- Transaction hash as binary
    tx_hash BINARY(32) NOT NULL,
    -- Data intent Uuid V4 as binary
    intent_id BINARY(16) NOT NULL,
    -- composite index for uniqueness of this tuple plus fast retrieval
    PRIMARY KEY (tx_hash, intent_id)
);

-- Index for fast retrieval of which tx_hash a given intent_id is in
CREATE INDEX idx_intent_id ON intents_tx_inclusion (intent_id);
