use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use ethers::{
    providers::{Http, Middleware, Provider},
    types::{Address, Block, Transaction, H256},
};
use eyre::{bail, eyre, Result};

use crate::blob_tx_data::BlobTxSummary;

type Nonce = u64;

pub struct BlockSync {
    unfinalized_head_chain: Vec<BlockSummary>,
    pending_transactions: HashMap<(Address, Nonce), BlobTxSummary>,
    target_address: Address,
}

#[async_trait]
pub trait BlockProvider {
    // async fn get_block_number(&self) -> Result<u64>;
    async fn get_block_by_hash(&self, block_hash: &H256) -> Result<Option<BlockWithTxs>>;
}

impl BlockSync {
    pub fn new(target_address: Address, anchor_block_hash: H256, anchor_block_number: u64) -> Self {
        let anchor_block = BlockSummary {
            hash: anchor_block_hash,
            number: anchor_block_number,
            // parent hash of anchor block does not matter
            parent_hash: [0u8; 32].into(),
            blob_txs: vec![],
            topup_txs: vec![],
        };
        Self {
            unfinalized_head_chain: vec![anchor_block],
            pending_transactions: <_>::default(),
            target_address,
        }
    }

    /// Returns the unfinalized balance delta for `address`, including both top-ups and included
    /// blob transactions
    pub fn unfinalized_balance_delta(&self, address: Address) -> i128 {
        let balance_delta_block_inclusions = self
            .unfinalized_head_chain
            .iter()
            .map(|block| block.balance_delta(address))
            .sum::<i128>();

        let cost_of_pending_txs = self
            .pending_transactions
            .values()
            .map(|tx| tx.cost_to_participant(address))
            .sum::<u128>();

        balance_delta_block_inclusions - cost_of_pending_txs as i128
    }

    pub fn register_pending_blob_tx(&mut self, tx: BlobTxSummary) {
        self.pending_transactions.insert((tx.from, tx.nonce), tx);
    }

    pub async fn sync_next_block<T: BlockProvider>(
        &mut self,
        provider: &T,
        block: BlockWithTxs,
    ) -> Result<()> {
        let block = BlockSummary::from_block(block, self.target_address)?;

        // let block_number = provider.get_block_number().await?;
        // let block = provider
        //   .get_block_with_txs(block_number)
        //   .await?
        //   .ok_or_else(|| eyre::eyre!("block at current head height should be present"))?;

        let last_block = self
            .unfinalized_head_chain
            .last()
            .ok_or_else(|| eyre!("there should always be one block"))?;

        if block.hash == last_block.hash
            || self
                .unfinalized_head_chain
                .iter()
                .find(|x| block.hash == x.hash)
                .is_some()
        {
            // Block already known
            return Ok(());
        } else if block.parent_hash == last_block.hash {
            // Next unknown block is descendant of head
            self.sync_block(block);
        } else {
            // Next unknown block is not descendant of head: re-org
            self.handle_reorg(provider, block).await?;
        }

        Ok(())
    }

    async fn handle_reorg<T: BlockProvider>(
        &mut self,
        provider: &T,
        block: BlockSummary,
    ) -> Result<()> {
        let mut new_blocks = vec![block];

        let common_ancestor_position = loop {
            let new_chain_ancestor = new_blocks.last().expect("should have at least one element");
            let new_chain_parent_hash = new_chain_ancestor.parent_hash;

            if let Some(index) = self
                .unfinalized_head_chain
                .iter()
                .position(|x| x.hash == new_chain_parent_hash)
            {
                break index;
            } else {
                if let Some(first_block) = self.unfinalized_head_chain.first() {
                    if new_chain_ancestor.number <= first_block.number {
                        bail!("re-org deeper than first known block")
                    }
                }

                let new_block = provider
                    .get_block_by_hash(&new_chain_parent_hash)
                    .await?
                    .ok_or_else(|| eyre::eyre!("parent block should be known"))?;
                new_blocks.push(BlockSummary::from_block(new_block, self.target_address)?);
            }
        };

        // do accounting on the balances cache
        let reorged_blocks = self
            .unfinalized_head_chain
            .iter()
            .skip(common_ancestor_position)
            .cloned()
            .collect::<Vec<_>>();

        let nonces_in_new_chain = new_blocks
            .iter()
            .flat_map(|b| b.blob_txs.iter().map(|tx| tx.nonce))
            .collect::<HashSet<_>>();

        for reorged_block in reorged_blocks {
            for tx in reorged_block.blob_txs {
                if !nonces_in_new_chain.contains(&tx.nonce) {
                    self.pending_transactions.insert((tx.from, tx.nonce), tx);
                }
            }
        }

        // TODO: move re-orged transactions not in the new chain back into the pending pool

        // for reorged_block in reorged_blocks {
        //     self.drop_reorged_blocks(&reorged_block);
        // }

        // All blocks in current chain after the pivot must be dropped
        self.unfinalized_head_chain
            .truncate(common_ancestor_position + 1);

        // Apply new blocks, starting from lowest height first
        for block in new_blocks.into_iter().rev() {
            self.sync_block(block);
        }

        Ok(())
    }

    fn sync_block(&mut self, block: BlockSummary) {
        // Drop pending transactions
        for tx in &block.blob_txs {
            self.pending_transactions.remove(&(tx.from, tx.nonce));
        }
        self.unfinalized_head_chain.push(block);
    }
}

#[derive(Clone, Debug)]
struct BlockSummary {
    number: u64,
    hash: H256,
    parent_hash: H256,
    topup_txs: Vec<TopupTx>,
    blob_txs: Vec<BlobTxSummary>,
}

#[derive(Clone, Debug)]
struct TopupTx {
    pub from: Address,
    pub value: u128,
}

impl BlockSummary {
    fn from_block(block: BlockWithTxs, target_address: Address) -> Result<Self> {
        let mut topup_txs = vec![];
        let mut blob_txs = vec![];

        for tx in block.transactions {
            if tx.to == Some(target_address) {
                topup_txs.push(TopupTx {
                    from: tx.from,
                    value: tx.value.as_u128(),
                });
            }

            // TODO: Handle invalid blob tx errors more gracefully
            if let Some(blob_tx) = BlobTxSummary::from_tx(tx, target_address)? {
                blob_txs.push(blob_tx)
            }
        }

        Ok(Self {
            number: block.number,
            hash: block.hash,
            parent_hash: block.parent_hash,
            topup_txs,
            blob_txs,
        })
    }

    fn balance_delta(&self, address: Address) -> i128 {
        let mut balance_delta = 0;

        for tx in &self.topup_txs {
            if tx.from == address {
                balance_delta += tx.value as i128;
            }
        }

        for blob_tx in &self.blob_txs {
            balance_delta -= blob_tx.cost_to_participant(address) as i128
        }

        balance_delta
    }
}

#[derive(Clone)]
pub struct BlockWithTxs {
    hash: H256,
    parent_hash: H256,
    number: u64,
    transactions: Vec<Transaction>,
}

impl BlockWithTxs {
    fn from_ethers_block(block: Block<Transaction>) -> Result<Self> {
        Ok(Self {
            number: block
                .number
                .ok_or_else(|| eyre!("block number not present"))?
                .as_u64(),
            hash: block.hash.ok_or_else(|| eyre!("block hash not present"))?,
            parent_hash: block.parent_hash,
            transactions: block.transactions,
        })
    }
}

#[async_trait]
impl BlockProvider for Provider<Http> {
    async fn get_block_by_hash(&self, block_hash: &H256) -> Result<Option<BlockWithTxs>> {
        let block = self.get_block_with_txs(*block_hash).await?;
        Ok(match block {
            Some(block) => Some(BlockWithTxs::from_ethers_block(block)?),
            None => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::blob_tx_data::{
        encode_blob_tx_data, BlobTxParticipant, BlobTxSummary, BLOB_TX_TYPE,
    };

    use super::{BlockProvider, BlockSync, BlockWithTxs};
    use async_trait::async_trait;
    use ethers::types::{Address, Transaction, H256};
    use eyre::Result;
    use std::{collections::HashMap, str::FromStr};

    #[derive(Default)]
    struct MockEthereumProvider {
        blocks: HashMap<H256, BlockWithTxs>,
    }

    impl MockEthereumProvider {
        fn with_blocks(blocks: &[BlockWithTxs]) -> Self {
            let mut sync = Self::default();
            for block in blocks {
                sync.blocks.insert(block.hash, block.clone());
            }
            sync
        }
    }

    #[async_trait]
    impl BlockProvider for MockEthereumProvider {
        async fn get_block_by_hash(&self, block_hash: &H256) -> Result<Option<BlockWithTxs>> {
            Ok(self.blocks.get(&block_hash).cloned())
        }
    }

    const ADDRESS_SENDER: &str = "0x0000000000000000000000000000000000000000";
    const SAMPLE_WEI_PER_BYTE: u128 = 1;

    #[tokio::test]
    async fn block_sync_simple_case() {
        let hash_0 = get_hash(0);
        let hash_1 = get_hash(1);
        let hash_2 = get_hash(2);
        let hash_3 = get_hash(3);
        let block_1 = generate_block(hash_0, hash_1, 1);
        let block_2 = generate_block(hash_1, hash_2, 2);
        let block_3 = generate_block(hash_2, hash_3, 3);

        let mut sync = BlockSync::new(Address::from_str(ADDRESS_SENDER).unwrap(), hash_0, 0);
        let provider = MockEthereumProvider::default();

        sync.sync_next_block(&provider, block_1).await.unwrap();
        sync.sync_next_block(&provider, block_2).await.unwrap();
        sync.sync_next_block(&provider, block_3).await.unwrap();
        assert_chain(&sync, &[hash_0, hash_1, hash_2, hash_3]);
    }

    #[tokio::test]
    async fn block_sync_reorg_with_common_ancestor() {
        let hash_0 = get_hash(0);
        let hash_1 = get_hash(1);
        let hash_2a = get_hash(0x2a);
        let hash_3a = get_hash(0x3a);
        let hash_2b = get_hash(0x2b);
        let hash_3b = get_hash(0x3b);
        let mut block_1 = generate_block(hash_0, hash_1, 1);
        let mut block_2a = generate_block(hash_1, hash_2a, 2);
        let mut block_3a = generate_block(hash_2a, hash_3a, 3);
        let block_2b = generate_block(hash_1, hash_2b, 2);
        let block_3b = generate_block(hash_2b, hash_3b, 3);

        let mut sync = BlockSync::new(Address::from_str(ADDRESS_SENDER).unwrap(), hash_0, 0);
        let provider = MockEthereumProvider::with_blocks(&[block_2b]);

        let user = Address::from_str("0x00000000000000000000000000000000000000aa").unwrap();
        const TOP_UP: i128 = 2000;
        const TXCOST: i128 = 500;
        const NONCE0: u64 = 0;
        const NONCE1: u64 = 1;

        // Register block with top-up transaction
        block_1.transactions.push(generate_topup_tx(user, TOP_UP));
        sync.sync_next_block(&provider, block_1).await.unwrap();
        assert_eq!(sync.unfinalized_balance_delta(user), TOP_UP);

        // Register pending tx for user
        sync.register_pending_blob_tx(generate_pending_blob_tx(NONCE0, user, TXCOST));
        assert_eq!(sync.unfinalized_balance_delta(user), 1500); // TOPUP - TXCOST
        assert_eq!(sync.pending_transactions.len(), 1);

        // Register blocks with blob txs, should not change the user balance
        block_2a
            .transactions
            .push(generate_blob_tx(NONCE0, user, TXCOST));
        block_3a.transactions.push(generate_topup_tx(user, TOP_UP));
        block_3a
            .transactions
            .push(generate_blob_tx(NONCE1, user, TXCOST));
        sync.sync_next_block(&provider, block_2a).await.unwrap();
        sync.sync_next_block(&provider, block_3a).await.unwrap();
        assert_chain(&sync, &[hash_0, hash_1, hash_2a, hash_3a]);
        assert_eq!(sync.unfinalized_balance_delta(user), 3000); // 2 * TOP_UP - 2 * TXCOST
        assert_eq!(sync.pending_transactions.len(), 0);

        // Trigger re-org with blocks that do not include the blob transactions, user balance
        // should not change since the transactions are re-added to the pool
        sync.sync_next_block(&provider, block_3b).await.unwrap();
        assert_chain(&sync, &[hash_0, hash_1, hash_2b, hash_3b]);
        assert_eq!(sync.unfinalized_balance_delta(user), 1000); // TOPUP - 2 * TXCOST
        assert_eq!(sync.pending_transactions.len(), 2);
    }

    #[tokio::test]
    async fn block_sync_reorg_no_common_ancestor() {
        let hash_0a = get_hash(0x0a);
        let hash_1a = get_hash(0x1a);
        let hash_2a = get_hash(0x2a);
        let hash_0b = get_hash(0x0b);
        let hash_1b = get_hash(0x1b);
        let hash_2b = get_hash(0x2b);
        let block_1a = generate_block(hash_0a, hash_1a, 1);
        let block_2a = generate_block(hash_1a, hash_2a, 2);
        let block_0b = generate_block(hash_0b, hash_0b, 0);
        let block_1b = generate_block(hash_0b, hash_1b, 1);
        let block_2b = generate_block(hash_1b, hash_2b, 2);

        let mut sync = BlockSync::new(Address::from_str(ADDRESS_SENDER).unwrap(), hash_0a, 0);
        let provider = MockEthereumProvider::with_blocks(&[block_0b, block_1b]);

        sync.sync_next_block(&provider, block_1a).await.unwrap();
        sync.sync_next_block(&provider, block_2a).await.unwrap();
        assert_chain(&sync, &[hash_0a, hash_1a, hash_2a]);

        assert_eq!(
            sync.sync_next_block(&provider, block_2b)
                .await
                .unwrap_err()
                .to_string(),
            "re-org deeper than first known block"
        )
    }

    fn get_hash(i: u64) -> H256 {
        H256::from_low_u64_be(i)
    }

    fn generate_block(parent_hash: H256, hash: H256, number: u64) -> BlockWithTxs {
        BlockWithTxs {
            number,
            hash,
            parent_hash,
            transactions: vec![],
        }
    }

    fn generate_pending_blob_tx(nonce: u64, participant: Address, data_len: i128) -> BlobTxSummary {
        let participant = BlobTxParticipant {
            participant,
            data_len: data_len as u64,
        };
        BlobTxSummary {
            tx_hash: [0u8; 32].into(),
            from: Address::from_str(ADDRESS_SENDER).unwrap(),
            nonce,
            participants: vec![participant],
            wei_per_byte: SAMPLE_WEI_PER_BYTE,
        }
    }

    fn generate_blob_tx(nonce: u64, participant: Address, data_len: i128) -> Transaction {
        let mut tx = Transaction::default();
        tx.from = Address::from_str(ADDRESS_SENDER).unwrap();
        tx.nonce = nonce.into();
        // TODO: use actual eip4844 pricing
        tx.gas_price = Some(SAMPLE_WEI_PER_BYTE.into());
        tx.transaction_type = Some(BLOB_TX_TYPE.into());
        tx.input = encode_blob_tx_data(&[BlobTxParticipant {
            participant,
            data_len: data_len as u64,
        }])
        .into();
        tx
    }

    fn generate_topup_tx(participant: Address, value: i128) -> Transaction {
        let mut tx = Transaction::default();
        tx.from = participant;
        tx.to = Some(Address::from_str(ADDRESS_SENDER).unwrap());
        tx.value = value.into();
        tx
    }

    fn assert_chain(sync: &BlockSync, expected_hash_chain: &[H256]) {
        let hash_chain = sync
            .unfinalized_head_chain
            .iter()
            .map(|b| b.hash)
            .collect::<Vec<_>>();
        assert_eq!(hash_chain, expected_hash_chain);
    }
}
