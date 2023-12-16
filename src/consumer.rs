use std::collections::HashSet;

use crate::{
    beacon_api_client::{BeaconApiClient, Slot},
    blob_tx_data::{is_blob_tx, BlobTxSummary},
    kzg::decode_blob_to_data,
};
use ethers::{
    providers::{Http, Middleware, Provider},
    types::{Address, Block, Transaction, H256},
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

        let datas_from_blob = blob_sidecars
            .data
            .iter()
            .map(|item| decode_blob_to_data(&item.blob))
            .collect::<Vec<_>>();

        // TODO extend to more
        assert_eq!(blob_sidecars.data.len(), 1);

        Ok(participants
            .iter()
            .map(|item| {
                datas_from_blob[0][item.data_offset..item.data_offset + item.data_len].to_vec()
            })
            .collect::<Vec<_>>())
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
    // Track all blob transactions in the blob
    let mut block_blob_index = 0;
    let mut block_data_chunks = vec![];

    for tx in &block.transactions {
        if is_blob_tx(tx) {
            let blob_index = block_blob_index;
            block_blob_index += 1;

            match BlobTxSummary::from_tx(tx, target_address) {
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

    block_data_chunks
}

#[cfg(test)]
mod tests {
    use crate::MAX_USABLE_BLOB_DATA_LEN;

    #[test]
    fn extract_data() {
        let data_1 = vec![0xaa_u8; MAX_USABLE_BLOB_DATA_LEN / 3];
        let data_2 = vec![0xbb_u8; MAX_USABLE_BLOB_DATA_LEN / 2 - 1];
    }
}
