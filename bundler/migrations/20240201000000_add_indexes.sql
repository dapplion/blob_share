-- Add index on eth_address for per-user queries (balance, intent listing)
CREATE INDEX idx_data_intents_eth_address ON data_intents(eth_address);

-- Add index on anchor_block.block_number for ORDER BY block_number DESC LIMIT 1
CREATE INDEX idx_anchor_block_number ON anchor_block(block_number DESC);
