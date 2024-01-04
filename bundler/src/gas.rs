use bundler_client::types::BlockGasSummary;
use ethers::types::{Block, TxHash};
use eyre::{eyre, Result};
use std::cmp;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum FeeEstimator {
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GasConfig {
    pub max_priority_fee_per_gas: u128,
    pub max_fee_per_gas: u128,
    pub max_fee_per_blob_gas: u128,
}

impl GasConfig {
    /// Bump a GasConfig to re-price by at least +110% another gas config from a previous
    /// transaction.
    /// Ref: <https://docs.alchemy.com/docs/retrying-an-eip-1559-transaction>
    pub fn reprice_to_at_least(&mut self, other_gas: GasConfig, bump_percent: u128) {
        self.max_priority_fee_per_gas = cmp::max(
            self.max_priority_fee_per_gas,
            ((100 + bump_percent) * other_gas.max_priority_fee_per_gas) / 100,
        );
        self.max_fee_per_gas = cmp::max(
            self.max_fee_per_gas, //
            ((100 + bump_percent) * other_gas.max_fee_per_gas) / 100,
        );
        self.max_fee_per_blob_gas = cmp::max(
            self.max_fee_per_blob_gas,
            ((100 + bump_percent) * other_gas.max_fee_per_blob_gas) / 100,
        );
    }

    /// Returns true if this gas config is underpriced against a block gas summary
    pub fn is_underpriced(&self, block_gas: &BlockGasSummary) -> bool {
        // TODO: check priority fee too
        // EVM gas underpriced
        block_gas.base_fee_per_gas > self.max_fee_per_gas
        // Blob gas underpriced
            || block_gas.blob_gas_price() > self.max_fee_per_blob_gas
    }
}

pub fn block_gas_summary_from_block(block: &Block<TxHash>) -> Result<BlockGasSummary> {
    Ok(BlockGasSummary {
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
            .ok_or_else(|| eyre!("block missing prop excess_blob_gas"))?
            .as_u128(),
    })
}
