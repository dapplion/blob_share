use std::collections::{HashMap, HashSet};

use crate::{
    beacon_api_client::{BeaconApiClient, Slot},
    blob_tx_data::{is_blob_tx, BlobTxSummary},
    kzg::decode_blob_to_data,
    routes::get_blobs::{BlobParticipantData, BlobsResponse},
    utils::address_to_hex_lowercase,
};
use ethers::{
    providers::{Http, Middleware, Provider},
    types::{Address, Block, Transaction, TxHash, H256},
};
use eyre::{bail, eyre, Result};

pub struct BlobConsumer {
    beacon_api_client: BeaconApiClient,
    eth_provider: Provider<Http>,
    target_address: Address,
    participant_address: Address,
}

impl BlobConsumer {
    pub fn new(
        beacon_api_base_url: &str,
        eth_provider_base_url: &str,
        target_address: Address,
        participant_address: Address,
    ) -> Result<Self> {
        Ok(Self {
            beacon_api_client: BeaconApiClient::new(beacon_api_base_url)?,
            eth_provider: Provider::<Http>::try_from(eth_provider_base_url)?,
            target_address,
            participant_address,
        })
    }

    pub async fn extract_data_participation_from_block_hash(
        &self,
        block_hash: H256,
    ) -> Result<Vec<Vec<u8>>> {
        let block = self
            .eth_provider
            .get_block_with_txs(block_hash)
            .await?
            .ok_or_else(|| eyre!("no block found for hash {}", block_hash))?;
        self.extract_data_participation_from_block(&block).await
    }

    pub async fn extract_data_participation_from_block(
        &self,
        block: &Block<Transaction>,
    ) -> Result<Vec<Vec<u8>>> {
        let participants = extract_data_participations_from_block(
            block,
            self.target_address,
            self.participant_address,
        );

        if participants.is_empty() {
            return Ok(vec![]);
        }

        let blob_indicies = participants
            .iter()
            .map(|p| p.blob_index)
            .collect::<HashSet<usize>>()
            .into_iter()
            .collect::<Vec<_>>();

        let block_slot = self.slot_of_block(block.timestamp.as_u64()).await?;

        let blob_sidecars = self
            .beacon_api_client
            .get_blob_sidecars(block_slot, Some(&blob_indicies))
            .await?;

        // Build a map from blob index to decoded blob data to handle multi-blob transactions
        let blob_data_by_index: HashMap<usize, Vec<u8>> = blob_sidecars
            .data
            .iter()
            .map(|item| (item.index as usize, decode_blob_to_data(&item.blob)))
            .collect();

        participants
            .iter()
            .map(|item| {
                let blob_data = blob_data_by_index.get(&item.blob_index).ok_or_else(|| {
                    eyre!(
                        "blob index {} not found in sidecars response",
                        item.blob_index
                    )
                })?;
                let end = item.data_offset + item.data_len;
                if end > blob_data.len() {
                    bail!(
                        "data range {}..{} exceeds blob {} decoded length {}",
                        item.data_offset,
                        end,
                        item.blob_index,
                        blob_data.len()
                    );
                }
                Ok(blob_data[item.data_offset..end].to_vec())
            })
            .collect::<Result<Vec<_>>>()
    }

    /// Fetch blob data for a given transaction hash. Returns participants with their decoded
    /// data extracted from the blob sidecars.
    pub async fn fetch_blob_data_by_tx_hash(&self, tx_hash: TxHash) -> Result<BlobsResponse> {
        let tx = self
            .eth_provider
            .get_transaction(tx_hash)
            .await?
            .ok_or_else(|| eyre!("transaction {tx_hash:?} not found"))?;

        if !is_blob_tx(&tx) {
            bail!("transaction {tx_hash:?} is not a blob transaction");
        }

        let block_hash = tx
            .block_hash
            .ok_or_else(|| eyre!("transaction {tx_hash:?} is not yet included in a block"))?;

        let block = self
            .eth_provider
            .get_block_with_txs(block_hash)
            .await?
            .ok_or_else(|| eyre!("block {block_hash:?} not found"))?;

        let blob_tx_summary = BlobTxSummary::from_tx(&tx)?
            .ok_or_else(|| eyre!("could not parse blob tx summary from {tx_hash:?}"))?;

        // Find the blob index for this transaction within the block
        let mut block_blob_index: usize = 0;
        let mut tx_blob_index: Option<usize> = None;
        for block_tx in &block.transactions {
            if block_tx.hash == tx_hash {
                tx_blob_index = Some(block_blob_index);
                break;
            }
            if is_blob_tx(block_tx) {
                block_blob_index += 1;
            }
        }
        let tx_blob_index =
            tx_blob_index.ok_or_else(|| eyre!("transaction {tx_hash:?} not found in its block"))?;

        let block_slot = self.slot_of_block(block.timestamp.as_u64()).await?;
        let blob_sidecars = self
            .beacon_api_client
            .get_blob_sidecars(block_slot, Some(&[tx_blob_index]))
            .await?;

        let blob_data = blob_sidecars
            .data
            .first()
            .ok_or_else(|| eyre!("no blob sidecar returned for index {tx_blob_index}"))?;
        let decoded = decode_blob_to_data(&blob_data.blob);

        let mut data_offset: usize = 0;
        let mut participants = Vec::with_capacity(blob_tx_summary.participants.len());
        for p in &blob_tx_summary.participants {
            let end = data_offset + p.data_len;
            let data = if end <= decoded.len() {
                decoded[data_offset..end].to_vec()
            } else {
                // Return what's available, capped to decoded length
                decoded[data_offset..decoded.len().min(end)].to_vec()
            };
            participants.push(BlobParticipantData {
                address: address_to_hex_lowercase(p.address),
                data_len: p.data_len,
                data,
            });
            data_offset = end;
        }

        Ok(BlobsResponse {
            tx_hash,
            participants,
        })
    }

    async fn slot_of_block(&self, timestamp: u64) -> Result<Slot> {
        let genesis = self.beacon_api_client.get_genesis().await?;
        let spec = self.beacon_api_client.get_spec().await?;

        // Return error for better visibility, should never happen in normal usage
        if timestamp < genesis.data.genesis_time {
            bail!(
                "block timestamp {} is before consensus genesis {}",
                timestamp,
                genesis.data.genesis_time
            );
        }

        let seconds_since_genesis = timestamp.saturating_sub(genesis.data.genesis_time);
        Ok((seconds_since_genesis / spec.data.SECONDS_PER_SLOT).into())
    }
}

pub struct BlockDataChunk {
    blob_index: usize,
    data_offset: usize,
    data_len: usize,
}

pub fn extract_data_participations_from_block(
    block: &Block<Transaction>,
    target_address: Address,
    participant_address: Address,
) -> Vec<BlockDataChunk> {
    let mut block_blob_index = 0;
    let mut block_data_chunks = vec![];

    for tx in &block.transactions {
        // Track all blob transactions in the blob
        if is_blob_tx(tx) {
            let blob_index = block_blob_index;
            block_blob_index += 1;

            if tx.from == target_address {
                match BlobTxSummary::from_tx(tx) {
                    Ok(None) => {}
                    // Ignore errors for invalid transactions
                    Err(_) => {}
                    Ok(Some(blob_tx)) => {
                        let mut blob_data_ptr = 0;

                        for participant in &blob_tx.participants {
                            let data_offset = blob_data_ptr;
                            blob_data_ptr += participant.data_len;

                            if participant.address == participant_address {
                                block_data_chunks.push(BlockDataChunk {
                                    blob_index,
                                    data_offset,
                                    data_len: participant.data_len,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    block_data_chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_tx_data::{encode_blob_tx_data, BlobTxParticipant, BLOB_TX_TYPE};

    fn gen_addr(b: u8) -> Address {
        [b; 20].into()
    }

    /// Create a blob transaction with the given participants, sender, and fees.
    fn make_blob_tx(from: Address, participants: &[BlobTxParticipant]) -> Transaction {
        let input = encode_blob_tx_data(participants).unwrap();
        let mut tx = Transaction::default();
        tx.transaction_type = Some(BLOB_TX_TYPE.into());
        tx.from = from;
        tx.input = input.into();
        tx.max_fee_per_gas = Some(1.into());
        tx.max_priority_fee_per_gas = Some(1.into());
        tx.other.insert("maxFeePerBlobGas".to_string(), "1".into());
        tx
    }

    /// Create a non-blob (type 2) transaction.
    fn make_regular_tx(from: Address) -> Transaction {
        let mut tx = Transaction::default();
        tx.transaction_type = Some(2.into());
        tx.from = from;
        tx
    }

    fn make_block(transactions: Vec<Transaction>) -> Block<Transaction> {
        Block {
            transactions,
            ..Default::default()
        }
    }

    #[test]
    fn extract_participations_empty_block() {
        let block = make_block(vec![]);
        let chunks = extract_data_participations_from_block(&block, gen_addr(0xAA), gen_addr(0xBB));
        assert_eq!(chunks.len(), 0);
    }

    #[test]
    fn extract_participations_no_blob_txs() {
        let block = make_block(vec![make_regular_tx(gen_addr(0xAA))]);
        let chunks = extract_data_participations_from_block(&block, gen_addr(0xAA), gen_addr(0xBB));
        assert_eq!(chunks.len(), 0);
    }

    #[test]
    fn extract_participations_blob_tx_wrong_sender() {
        let participants = vec![BlobTxParticipant {
            address: gen_addr(0xBB),
            data_len: 1000,
        }];
        let block = make_block(vec![make_blob_tx(gen_addr(0xCC), &participants)]);
        // target_address is 0xAA, but tx is from 0xCC
        let chunks = extract_data_participations_from_block(&block, gen_addr(0xAA), gen_addr(0xBB));
        assert_eq!(chunks.len(), 0);
    }

    #[test]
    fn extract_participations_no_matching_participant() {
        let target = gen_addr(0xAA);
        let participants = vec![BlobTxParticipant {
            address: gen_addr(0xCC),
            data_len: 500,
        }];
        let block = make_block(vec![make_blob_tx(target, &participants)]);
        // Looking for participant 0xBB, but only 0xCC is in the tx
        let chunks = extract_data_participations_from_block(&block, target, gen_addr(0xBB));
        assert_eq!(chunks.len(), 0);
    }

    #[test]
    fn extract_participations_single_participant_match() {
        let target = gen_addr(0xAA);
        let participant = gen_addr(0xBB);
        let participants = vec![BlobTxParticipant {
            address: participant,
            data_len: 2000,
        }];
        let block = make_block(vec![make_blob_tx(target, &participants)]);
        let chunks = extract_data_participations_from_block(&block, target, participant);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].blob_index, 0);
        assert_eq!(chunks[0].data_offset, 0);
        assert_eq!(chunks[0].data_len, 2000);
    }

    #[test]
    fn extract_participations_multiple_participants_one_match() {
        let target = gen_addr(0xAA);
        let participant = gen_addr(0xBB);
        let participants = vec![
            BlobTxParticipant {
                address: gen_addr(0xCC),
                data_len: 3000,
            },
            BlobTxParticipant {
                address: participant,
                data_len: 1500,
            },
        ];
        let block = make_block(vec![make_blob_tx(target, &participants)]);
        let chunks = extract_data_participations_from_block(&block, target, participant);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].blob_index, 0);
        // data_offset = 3000 (after first participant's data)
        assert_eq!(chunks[0].data_offset, 3000);
        assert_eq!(chunks[0].data_len, 1500);
    }

    #[test]
    fn extract_participations_multiple_matches_same_tx() {
        let target = gen_addr(0xAA);
        let participant = gen_addr(0xBB);
        let participants = vec![
            BlobTxParticipant {
                address: participant,
                data_len: 1000,
            },
            BlobTxParticipant {
                address: gen_addr(0xCC),
                data_len: 500,
            },
            BlobTxParticipant {
                address: participant,
                data_len: 2000,
            },
        ];
        let block = make_block(vec![make_blob_tx(target, &participants)]);
        let chunks = extract_data_participations_from_block(&block, target, participant);

        assert_eq!(chunks.len(), 2);
        // First match at offset 0
        assert_eq!(chunks[0].blob_index, 0);
        assert_eq!(chunks[0].data_offset, 0);
        assert_eq!(chunks[0].data_len, 1000);
        // Second match after 1000 + 500
        assert_eq!(chunks[1].blob_index, 0);
        assert_eq!(chunks[1].data_offset, 1500);
        assert_eq!(chunks[1].data_len, 2000);
    }

    #[test]
    fn extract_participations_blob_index_increments_across_blob_txs() {
        let target = gen_addr(0xAA);
        let participant = gen_addr(0xBB);

        // First blob tx from a different sender (counts toward blob index)
        let other_tx = make_blob_tx(
            gen_addr(0xDD),
            &[BlobTxParticipant {
                address: gen_addr(0xEE),
                data_len: 500,
            }],
        );

        // Second blob tx from target with our participant
        let target_tx = make_blob_tx(
            target,
            &[BlobTxParticipant {
                address: participant,
                data_len: 1000,
            }],
        );

        let block = make_block(vec![other_tx, target_tx]);
        let chunks = extract_data_participations_from_block(&block, target, participant);

        assert_eq!(chunks.len(), 1);
        // blob_index should be 1 because the first blob tx incremented it
        assert_eq!(chunks[0].blob_index, 1);
        assert_eq!(chunks[0].data_offset, 0);
        assert_eq!(chunks[0].data_len, 1000);
    }

    #[test]
    fn extract_participations_regular_tx_does_not_increment_blob_index() {
        let target = gen_addr(0xAA);
        let participant = gen_addr(0xBB);

        // A regular (non-blob) tx should not increment the blob index
        let regular_tx = make_regular_tx(gen_addr(0xDD));

        let target_tx = make_blob_tx(
            target,
            &[BlobTxParticipant {
                address: participant,
                data_len: 1000,
            }],
        );

        let block = make_block(vec![regular_tx, target_tx]);
        let chunks = extract_data_participations_from_block(&block, target, participant);

        assert_eq!(chunks.len(), 1);
        // blob_index should still be 0 because the regular tx didn't count
        assert_eq!(chunks[0].blob_index, 0);
    }

    #[test]
    fn blob_consumer_new_valid_urls() {
        let consumer = BlobConsumer::new(
            "http://localhost:5052",
            "http://localhost:8545",
            gen_addr(0xAA),
            gen_addr(0xBB),
        );
        assert!(consumer.is_ok());
    }

    #[test]
    fn blob_consumer_new_invalid_beacon_url() {
        let consumer = BlobConsumer::new(
            "not-a-valid-url",
            "http://localhost:8545",
            gen_addr(0xAA),
            gen_addr(0xBB),
        );
        assert!(consumer.is_err());
    }

    #[test]
    fn blob_consumer_new_invalid_eth_url() {
        let consumer = BlobConsumer::new(
            "http://localhost:5052",
            "not-a-valid-url",
            gen_addr(0xAA),
            gen_addr(0xBB),
        );
        assert!(consumer.is_err());
    }
}
