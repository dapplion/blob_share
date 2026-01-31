-- Multi-blob data splitting: link chunks from the same large submission.
-- group_id is shared across all chunks from a single split; NULL for single-intent submissions.
ALTER TABLE data_intents ADD COLUMN group_id BINARY(16) NULL DEFAULT NULL;
-- chunk_index is the 0-based position within a group; NULL for single-intent submissions.
ALTER TABLE data_intents ADD COLUMN chunk_index SMALLINT UNSIGNED NULL DEFAULT NULL;
-- Index for group lookups
CREATE INDEX idx_data_intents_group_id ON data_intents(group_id);
