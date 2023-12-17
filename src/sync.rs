use std::{
    collections::{hash_map::Entry, HashMap},
    fmt, mem,
};

use async_trait::async_trait;
use ethers::{
    providers::{Http, Middleware, Provider, Ws},
    types::{Address, Block, Transaction, TxHash, H256},
};
use eyre::{bail, eyre, Result};
use tokio::sync::RwLock;

use crate::{blob_tx_data::BlobTxSummary, info, BlockGasSummary};

type Nonce = u64;

const FINALIZE_DEPTH: u64 = 2 * 32;

/// Represents the canonical view of the target chain to publish blob transactions.
/// Given a sequence of head blocks, it allows to query:
///
/// - the balance delta of users
/// - transaction status
/// - (bonus) the current head's blob gas price
///
/// It must handle:
/// - Re-orgs that drop transactions
/// - New block that under-prices pending transaction
///
/// # TX sequence
///
/// 1. Send tx with current head's next blob gas price
/// 2. Wait for next block, tx included?
///   2.1. Tx included => Ok
///   2.2. Tx not included, blob gas price increase?
///     2.2.1. Increase => cancel tx
///     2.2.2. Same or decrease => Ok
/// 3. Wait for re-org
///   3.1. Tx included in new head chain => Ok
///   3.2. Tx dropped => jump to 2.2.
///
pub struct BlockSync {
    anchor_block: AnchorBlock,
    unfinalized_head_chain: Vec<BlockSummary>,
    pending_transactions: HashMap<Nonce, BlobTxSummary>,
    target_address: Address,
}

#[async_trait]
pub trait BlockProvider {
    // async fn get_block_number(&self) -> Result<u64>;
    async fn get_block_by_hash(&self, block_hash: &H256) -> Result<Option<BlockWithTxs>>;
}

pub enum TxInclusion {
    Pending,
    Included(H256),
}

impl BlockSync {
    pub fn new(target_address: Address, anchor_block: AnchorBlock) -> Self {
        Self {
            anchor_block,
            unfinalized_head_chain: <_>::default(),
            pending_transactions: <_>::default(),
            target_address,
        }
    }

    /// Return gas summary of the current head
    pub fn get_head_gas(&self) -> &BlockGasSummary {
        if let Some(head) = self.unfinalized_head_chain.last() {
            &head.gas
        } else {
            &self.anchor_block.gas
        }
    }

    fn get_head_number(&self) -> u64 {
        if let Some(head) = self.unfinalized_head_chain.last() {
            head.number
        } else {
            self.anchor_block.number
        }
    }

    pub fn transaction_count_at_head(&self, target_address: Address) -> u64 {
        // TODO: Support multiple sender addresses
        assert_eq!(target_address, self.target_address);

        let unfinalized_tx_count = self
            .unfinalized_head_chain
            .iter()
            .map(|b| b.blob_txs.len())
            .sum::<usize>();
        self.anchor_block.target_address_nonce + unfinalized_tx_count as u64
    }

    pub fn transaction_count_at_head_with_pending_tx(&self, target_address: Address) -> u64 {
        // TODO: Support multiple sender addresses
        assert_eq!(target_address, self.target_address);

        self.transaction_count_at_head(target_address) + self.pending_transactions.len() as u64
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
            .map(|tx| tx.cost_to_participant(address, None, None))
            .sum::<u128>();

        balance_delta_block_inclusions - cost_of_pending_txs as i128
    }

    pub fn get_tx_status(&self, tx_hash: TxHash) -> Option<TxInclusion> {
        for block in self.unfinalized_head_chain.iter() {
            for tx in &block.blob_txs {
                if tx_hash == tx.tx_hash {
                    return Some(TxInclusion::Included(block.hash));
                }
            }
        }

        for tx in self.pending_transactions.values() {
            if tx_hash == tx.tx_hash {
                return Some(TxInclusion::Pending);
            }
        }

        None
    }

    /// Register pending transaction with sync
    pub fn register_pending_blob_tx(&mut self, tx: BlobTxSummary) -> Result<()> {
        // TODO: Handle multiple senders
        assert_eq!(tx.from, self.target_address);

        // TODO: Allow to replace transactions with same nonce. Note there's potential nonce
        // deadlocks where the blob sharing sends a viable transaction at gas price P, the network
        // base fee increases to 2*P and then there's no set of intents that can afford 2*P. In
        // that case the sender account is stuck because it can't send a profitable re-price
        // transaction. In that case it should send a self transfer to unlock the nonce.

        match self.pending_transactions.entry(tx.nonce) {
            Entry::Vacant(e) => {
                e.insert(tx);
                Ok(())
            }
            Entry::Occupied(_) => bail!("transaction already registered for nonce {}", tx.nonce),
        }
    }

    /// Removes and returns underpriced pending transactions against current head
    pub fn evict_underpriced_pending_txs(&mut self) -> Vec<BlobTxSummary> {
        let head_gas = self.get_head_gas();

        let keys_to_remove = self
            .pending_transactions
            .iter()
            .filter_map(|(k, tx)| {
                if tx.is_underpriced(head_gas) {
                    Some(*k)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        keys_to_remove
            .iter()
            .map(|k| self.pending_transactions.remove(k).expect("key must exist"))
            .collect()
    }

    /// Advance anchor block if distance with head is greater than FINALIZE_DEPTH
    pub fn maybe_advance_anchor_block(&mut self) -> Result<Option<Vec<BlobTxSummary>>> {
        let head_number = self.get_head_number();
        let new_anchor_index = if self.anchor_block_number() < head_number - FINALIZE_DEPTH {
            self.unfinalized_head_chain
                .iter()
                .position(|b| b.number == head_number - FINALIZE_DEPTH)
                .ok_or_else(|| {
                    eyre!(
                        "inconsistent unfinalized chain, block number {} missing",
                        head_number - FINALIZE_DEPTH
                    )
                })?
        } else {
            return Ok(None);
        };

        // split_off retains in the original array the first N items. If N = 0, the anchor
        // block is the first item in the unfinalized chain, so we should split_off at 1.
        let unfinalized_head_chain = self.unfinalized_head_chain.split_off(new_anchor_index + 1);
        let finalized_chain =
            mem::replace(&mut self.unfinalized_head_chain, unfinalized_head_chain);

        for block in &finalized_chain {
            self.anchor_block.apply_finalized_block(block);
        }

        Ok(Some(
            finalized_chain
                .into_iter()
                .flat_map(|b| b.blob_txs.into_iter())
                .collect(),
        ))
    }

    /// Register a new head block with sync. The new head's parent can be unknown. This function
    /// will recursively fetch all ancenstors until finding a common parent. Does not guarantee
    /// consistency where the entire chain of `block` is imported.
    pub async fn sync_next_head<T: BlockProvider>(
        sync: &RwLock<BlockSync>,
        provider: &T,
        block: BlockWithTxs,
    ) -> Result<SyncBlockOutcome, SyncBlockError> {
        let mut new_blocks = vec![block];

        loop {
            let new_chain_ancestor = new_blocks.last().expect("should have at least one element");
            let new_chain_parent_hash = new_chain_ancestor.parent_hash;

            if sync.read().await.is_known_block(new_chain_parent_hash) {
                break;
            }

            let anchor_block_number = sync.read().await.anchor_block_number();
            if new_chain_ancestor.number <= anchor_block_number {
                return Err(SyncBlockError::ReorgTooDeep {
                    anchor_block_number,
                });
            }

            let new_block = provider
                .get_block_by_hash(&new_chain_parent_hash)
                .await?
                .ok_or_else(|| eyre!("parent block {} should be known", new_chain_parent_hash))?;
            new_blocks.push(new_block);
        }

        let mut sync = sync.write().await;

        let outcomes = new_blocks
            .into_iter()
            .rev()
            .map(|block| sync.sync_next_head_parent_known(block))
            .collect::<Result<Vec<_>>>()?;

        Ok(SyncBlockOutcome::from_many(&outcomes))
    }

    /// Register a new block with sync. New blocks MUST be a descendant of a known block. To handle
    /// re-orgs gracefully, the caller should recursively fetch all necessary blocks until finding
    /// a known ancenstor.
    fn sync_next_head_parent_known(&mut self, block: BlockWithTxs) -> Result<SyncBlockOutcome> {
        let block = BlockSummary::from_block(block, self.target_address)?;

        if self.is_known_block(block.hash) {
            // Block already known
            Ok(SyncBlockOutcome::BlockKnown)
        } else if let Some(new_head_index) = self.block_position_plus_one(block.parent_hash) {
            let reorg = if new_head_index < self.unfinalized_head_chain.len() {
                // Next unknown block is not descendant of head: re-org
                self.drop_reorged_blocks(new_head_index);
                Some(Reorg {
                    depth: self.unfinalized_head_chain.len() - new_head_index,
                })
            } else {
                None
            };

            let blob_tx_hashes = block
                .blob_txs
                .iter()
                .map(|tx| tx.tx_hash)
                .collect::<Vec<_>>();

            self.sync_block(block);

            Ok(SyncBlockOutcome::Synced {
                reorg,
                blob_tx_hashes,
            })
        } else {
            bail!(
                "Unknown block parent {}, block hash {} number {}",
                block.parent_hash,
                block.hash,
                block.number
            )
        }
    }

    /// Returns true if block_hash is known in the current head chain
    fn is_known_block(&self, block_hash: H256) -> bool {
        self.block_position_plus_one(block_hash).is_some()
    }

    /// Returns the position of the block after `block_hash` if `block_hash` is known
    fn block_position_plus_one(&self, block_hash: H256) -> Option<usize> {
        if self.anchor_block.hash == block_hash {
            return Some(0);
        }
        self.unfinalized_head_chain
            .iter()
            .position(|b| b.hash == block_hash)
            .map(|i| i + 1)
    }

    /// Returns the block number of the anchor block. There should never exist a re-org deeper than
    /// this block number.
    fn anchor_block_number(&self) -> u64 {
        self.anchor_block.number
    }

    fn drop_reorged_blocks(&mut self, new_head_index: usize) {
        for reorged_block in self.unfinalized_head_chain.iter().skip(new_head_index) {
            // TODO: do accounting on the balances cache
            for tx in &reorged_block.blob_txs {
                self.pending_transactions.insert(tx.nonce, tx.clone());
            }
        }

        // All blocks in current chain after the pivot must be dropped
        self.unfinalized_head_chain.truncate(new_head_index);
    }

    fn sync_block(&mut self, block: BlockSummary) {
        // Drop pending transactions
        for tx in &block.blob_txs {
            self.pending_transactions.remove(&tx.nonce);
            info!(
                "pending blob tx included from {} nonce {} tx_hash {}",
                tx.from, tx.nonce, tx.tx_hash
            );
        }
        self.unfinalized_head_chain.push(block);
    }
}

#[derive(Debug)]
pub enum SyncBlockError {
    ReorgTooDeep { anchor_block_number: u64 },
    Other(eyre::Report),
}

impl fmt::Display for SyncBlockError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for SyncBlockError {
    fn cause(&self) -> Option<&dyn std::error::Error> {
        match self {
            SyncBlockError::ReorgTooDeep { .. } => None,
            SyncBlockError::Other(e) => Some(e.as_ref()),
        }
    }
}

impl From<eyre::ErrReport> for SyncBlockError {
    fn from(value: eyre::ErrReport) -> Self {
        Self::Other(value)
    }
}

#[derive(Debug, Clone)]
pub enum SyncBlockOutcome {
    BlockKnown,
    Synced {
        reorg: Option<Reorg>,
        blob_tx_hashes: Vec<TxHash>,
    },
}

impl SyncBlockOutcome {
    /// Collapse multiple outcomes into a single outcome
    /// - Expects either 1 or 0 Reorg item
    /// - BlockKnown are ignored if at least one Synced
    fn from_many(outcomes: &[SyncBlockOutcome]) -> Self {
        let mut synced_some = false;
        let mut blob_tx_hashes_all = vec![];
        let mut reorg_all: Option<Reorg> = None;

        for outcome in outcomes {
            if let SyncBlockOutcome::Synced {
                reorg,
                blob_tx_hashes,
            } = outcome
            {
                synced_some = true;
                blob_tx_hashes_all.extend_from_slice(blob_tx_hashes);
                if let Some(reorg) = reorg {
                    reorg_all = Some(reorg.clone());
                }
            }
        }

        if synced_some {
            SyncBlockOutcome::Synced {
                reorg: reorg_all,
                blob_tx_hashes: blob_tx_hashes_all,
            }
        } else {
            SyncBlockOutcome::BlockKnown
        }
    }
}

#[derive(Debug, Clone)]
pub struct Reorg {
    pub depth: usize,
}

#[derive(Clone, Debug)]
pub struct AnchorBlock {
    pub hash: H256,
    pub number: u64,
    pub target_address_nonce: u64,
    pub gas: BlockGasSummary,
    pub finalized_balances: HashMap<Address, i128>,
}

impl AnchorBlock {
    fn apply_finalized_block(&mut self, block: &BlockSummary) {
        for address in block.get_all_participants() {
            *self.finalized_balances.entry(address).or_insert(0) += block.balance_delta(address);
        }

        self.target_address_nonce += block.blob_txs.len() as u64;

        self.hash = block.hash;
        self.number = block.number;
        self.gas = block.gas.clone();
    }
}

#[derive(Clone, Debug)]
struct BlockSummary {
    number: u64,
    hash: H256,
    parent_hash: H256,
    topup_txs: Vec<TopupTx>,
    blob_txs: Vec<BlobTxSummary>,
    gas: BlockGasSummary,
}

#[derive(Clone, Debug)]
struct TopupTx {
    pub from: Address,
    pub value: u128,
}

impl TopupTx {
    fn is_from(&self, address: Address) -> bool {
        self.from == address
    }
}

impl BlockSummary {
    fn from_block(block: BlockWithTxs, target_address: Address) -> Result<Self> {
        let mut topup_txs = vec![];
        let mut blob_txs = vec![];

        for tx in &block.transactions {
            if tx.to == Some(target_address) {
                topup_txs.push(TopupTx {
                    from: tx.from,
                    value: tx.value.as_u128(),
                });
            }

            // TODO: Handle invalid blob tx errors more gracefully
            if tx.from == target_address {
                if let Some(blob_tx) = BlobTxSummary::from_tx(tx)? {
                    blob_txs.push(blob_tx)
                }
                // TODO: Handle target address sending non-blob transactions for correct nonce
                // accounting
            }
        }

        Ok(Self {
            number: block.number,
            hash: block.hash,
            parent_hash: block.parent_hash,
            topup_txs,
            blob_txs,
            gas: block.into(),
        })
    }

    fn balance_delta(&self, address: Address) -> i128 {
        let mut balance_delta = 0;

        for tx in &self.topup_txs {
            if tx.is_from(address) {
                balance_delta += tx.value as i128;
            }
        }

        for blob_tx in &self.blob_txs {
            balance_delta -= blob_tx.cost_to_participant(
                address,
                Some(self.gas.base_fee_per_gas),
                Some(self.gas.blob_gas_price()),
            ) as i128
        }

        balance_delta
    }

    /// Return all user addresses from topups and blob tx participations
    fn get_all_participants(&self) -> Vec<Address> {
        self.topup_txs
            .iter()
            .map(|tx| tx.from)
            .chain(
                self.blob_txs
                    .iter()
                    .flat_map(|tx| tx.participants.iter().map(|p| p.address)),
            )
            .collect()
    }
}

#[derive(Clone)]
pub struct BlockWithTxs {
    hash: H256,
    parent_hash: H256,
    number: u64,
    transactions: Vec<Transaction>,
    pub base_fee_per_gas: u128,
    pub excess_blob_gas: u128,
    pub blob_gas_used: u128,
}

impl BlockWithTxs {
    pub fn from_ethers_block(block: Block<Transaction>) -> Result<Self> {
        Ok(Self {
            number: block
                .number
                .ok_or_else(|| eyre!("block number not present"))?
                .as_u64(),
            hash: block.hash.ok_or_else(|| eyre!("block hash not present"))?,
            parent_hash: block.parent_hash,
            transactions: block.transactions,
            base_fee_per_gas: block
                .base_fee_per_gas
                .ok_or_else(|| eyre!("block should be post-London no base_fee_per_gas"))?
                .as_u128(),
            blob_gas_used: block
                .blob_gas_used
                .ok_or_else(|| eyre!("block missing prop blob_gas_used"))?
                .as_u128(),
            excess_blob_gas: block
                .excess_blob_gas
                .ok_or_else(|| eyre!("block should be post-cancun no excess_blob_gas"))?
                .as_u128(),
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

#[async_trait]
impl BlockProvider for Provider<Ws> {
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
    use super::BlockSummary;
    use crate::{
        blob_tx_data::{encode_blob_tx_data, BlobTxParticipant, BlobTxSummary, BLOB_TX_TYPE},
        BlockGasSummary,
    };

    use super::{BlockProvider, BlockSync, BlockWithTxs};
    use async_trait::async_trait;
    use c_kzg::BYTES_PER_BLOB;
    use ethers::types::{Address, Transaction, H160, H256};
    use eyre::Result;
    use std::collections::HashMap;
    use tokio::sync::RwLock;

    #[test]
    fn parse_topup_transactions() {
        let mut block = generate_block(get_hash(0), get_hash(0), 0);
        let addr_1 = get_addr(1);
        let addr_2 = get_addr(2);

        block.transactions.push(generate_topup_tx(addr_1, 1));
        block.transactions.push(generate_topup_tx(addr_2, 2));
        block.transactions.push(generate_topup_tx(addr_2, 2));

        let summary = BlockSummary::from_block(block, ADDRESS_SENDER).unwrap();
        assert_eq!(summary.balance_delta(addr_1), 1);
        assert_eq!(summary.balance_delta(addr_2), 2 * 2);
    }

    #[tokio::test]
    async fn block_sync_simple_case() {
        let hash_0 = get_hash(0);
        let hash_1 = get_hash(1);
        let hash_2 = get_hash(2);
        let hash_3 = get_hash(3);
        let block_1 = generate_block(hash_0, hash_1, 1);
        let block_2 = generate_block(hash_1, hash_2, 2);
        let block_3 = generate_block(hash_2, hash_3, 3);

        let sync = new_block_sync(hash_0, 0);
        let provider = MockEthereumProvider::default();

        sync_next_block(&sync, &provider, block_1).await;
        sync_next_block(&sync, &provider, block_2).await;
        sync_next_block(&sync, &provider, block_3).await;
        assert_chain(&sync, &[hash_0, hash_1, hash_2, hash_3]).await;
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

        let sync = new_block_sync(hash_0, 0);
        let provider = MockEthereumProvider::with_blocks(&[block_2b]);

        let user = get_addr(1);
        const TOP_UP: i128 = 2 * BYTES_PER_BLOB as i128;
        const TXCOST: i128 = (BYTES_PER_BLOB + 16 * 24) as i128;
        const NONCE0: u64 = 0;
        const NONCE1: u64 = 1;

        // Register block with top-up transaction
        block_1.transactions.push(generate_topup_tx(user, TOP_UP));
        sync_next_block(&sync, &provider, block_1).await;
        assert_eq!(sync.read().await.unfinalized_balance_delta(user), TOP_UP);

        // Register pending tx for user
        sync.write()
            .await
            .register_pending_blob_tx(generate_pending_blob_tx(NONCE0, user))
            .unwrap();
        assert_eq!(
            sync.read().await.unfinalized_balance_delta(user),
            TOP_UP - TXCOST
        ); // TOPUP - TXCOST
        assert_eq!(pending_tx_len(&sync).await, 1);

        // Register blocks with blob txs, should not change the user balance
        block_2a.transactions.push(generate_blob_tx(NONCE0, user));
        block_3a.transactions.push(generate_topup_tx(user, TOP_UP));
        block_3a.transactions.push(generate_blob_tx(NONCE1, user));
        sync_next_block(&sync, &provider, block_2a).await;
        sync_next_block(&sync, &provider, block_3a).await;
        assert_chain(&sync, &[hash_0, hash_1, hash_2a, hash_3a]).await;
        assert_eq!(
            sync.read().await.unfinalized_balance_delta(user),
            2 * TOP_UP - 2 * TXCOST
        );
        assert_eq!(pending_tx_len(&sync).await, 0);

        // Trigger re-org with blocks that do not include the blob transactions, user balance
        // should not change since the transactions are re-added to the pool
        sync_next_block(&sync, &provider, block_3b).await;
        assert_chain(&sync, &[hash_0, hash_1, hash_2b, hash_3b]).await;
        assert_eq!(
            sync.read().await.unfinalized_balance_delta(user),
            TOP_UP - 2 * TXCOST
        );
        assert_eq!(pending_tx_len(&sync).await, 2);
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

        let sync = new_block_sync(hash_0a, 0);
        let provider = MockEthereumProvider::with_blocks(&[block_0b, block_1b]);

        sync_next_block(&sync, &provider, block_1a).await;
        sync_next_block(&sync, &provider, block_2a).await;
        assert_chain(&sync, &[hash_0a, hash_1a, hash_2a]).await;

        assert_eq!(
            BlockSync::sync_next_head(&sync, &provider, block_2b)
                .await
                .unwrap_err()
                .to_string(),
            "re-org deeper than first known block"
        )
    }

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

    const ADDRESS_SENDER: Address = H160([0xab; 20]);

    fn get_hash(i: u64) -> H256 {
        H256::from_low_u64_be(i)
    }

    fn get_addr(i: u8) -> H160 {
        H160([i; 20])
    }

    fn generate_block(parent_hash: H256, hash: H256, number: u64) -> BlockWithTxs {
        BlockWithTxs {
            number,
            hash,
            parent_hash,
            transactions: vec![],
            base_fee_per_gas: 1,
            excess_blob_gas: 1,
            blob_gas_used: 1,
        }
    }

    fn generate_pending_blob_tx(nonce: u64, participant: Address) -> BlobTxSummary {
        let participant = BlobTxParticipant {
            address: participant,
            // Note: this length is not possible but makes the math easy in the test
            data_len: BYTES_PER_BLOB,
        };
        BlobTxSummary {
            tx_hash: [0u8; 32].into(),
            from: ADDRESS_SENDER,
            nonce,
            participants: vec![participant],
            used_bytes: BYTES_PER_BLOB,
            max_priority_fee_per_gas: 0, // set to 0 to make effective gas fee always 1
            max_fee_per_gas: 1,
            max_fee_per_blob_gas: 1,
        }
    }

    fn generate_blob_tx(nonce: u64, participant: Address) -> Transaction {
        let mut tx = Transaction::default();
        tx.from = ADDRESS_SENDER;
        tx.nonce = nonce.into();
        tx.max_priority_fee_per_gas = Some(0.into()); // set to 0 to make effective gas fee always 1
        tx.max_fee_per_gas = Some(1.into());
        tx.other.insert("maxFeePerBlobGas".to_string(), "1".into());
        tx.transaction_type = Some(BLOB_TX_TYPE.into());
        tx.input = encode_blob_tx_data(&[BlobTxParticipant {
            address: participant,
            data_len: BYTES_PER_BLOB as usize,
        }])
        .unwrap()
        .into();
        tx
    }

    fn generate_topup_tx(participant: Address, value: i128) -> Transaction {
        let mut tx = Transaction::default();
        tx.from = participant;
        tx.to = Some(ADDRESS_SENDER);
        tx.value = value.into();
        tx
    }

    async fn assert_chain(sync: &RwLock<BlockSync>, expected_hash_chain: &[H256]) {
        let sync = sync.read().await;
        let mut hash_chain = vec![sync.anchor_block.hash];
        hash_chain.extend_from_slice(
            &sync
                .unfinalized_head_chain
                .iter()
                .map(|b| b.hash)
                .collect::<Vec<_>>(),
        );
        assert_eq!(hash_chain, expected_hash_chain);
    }

    async fn sync_next_block<T: BlockProvider>(
        sync: &RwLock<BlockSync>,
        provider: &T,
        block: BlockWithTxs,
    ) {
        BlockSync::sync_next_head(sync, provider, block)
            .await
            .unwrap();
    }

    async fn pending_tx_len(sync: &RwLock<BlockSync>) -> usize {
        sync.read().await.pending_transactions.len()
    }

    fn new_block_sync(hash: H256, number: u64) -> RwLock<BlockSync> {
        RwLock::new(BlockSync::new(
            ADDRESS_SENDER,
            super::AnchorBlock {
                hash,
                number,
                target_address_nonce: 0,
                gas: BlockGasSummary::default(),
                finalized_balances: <_>::default(),
            },
        ))
    }
}
