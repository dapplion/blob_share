ALTER TABLE data_intents ADD COLUMN cancelled BOOL NOT NULL DEFAULT FALSE;
CREATE INDEX idx_data_intents_cancelled ON data_intents(cancelled);
