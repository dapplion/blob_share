-- Allow data column to be NULL so finalized intents can be pruned (data set to NULL).
ALTER TABLE data_intents MODIFY COLUMN data MEDIUMBLOB NULL;

-- Track the anchor block number at which the intent was marked as finalized.
-- Used to determine when enough blocks have passed to prune the raw data.
ALTER TABLE data_intents ADD COLUMN finalized_block_number INT UNSIGNED NULL DEFAULT NULL;

-- Delete old anchor_block rows, keeping only the latest.
-- Going forward the application will upsert instead of insert.
DELETE FROM anchor_block
WHERE block_number < (SELECT max_bn FROM (SELECT MAX(block_number) AS max_bn FROM anchor_block) AS t);
