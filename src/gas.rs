use ethers::types::{Block, TxHash};
use eyre::{eyre, Result};
use serde::{Deserialize, Serialize};

use crate::sync::BlockWithTxs;

const MIN_BLOB_GASPRICE: u128 = 1;
const BLOB_GASPRICE_UPDATE_FRACTION: u128 = 3338477;
const TARGET_BLOB_GAS_PER_BLOCK: u128 = 393216;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum FeeEstimator {
    Default,
}

#[derive(Debug)]
pub struct GasConfig {
    pub max_priority_fee_per_gas: u128,
    pub max_fee_per_gas: u128,
    pub max_fee_per_blob_gas: u128,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BlockGasSummary {
    blob_gas_used: u128,
    excess_blob_gas: u128,
    pub base_fee_per_gas: u128,
}

impl BlockGasSummary {
    pub fn from_block(block: &Block<TxHash>) -> Result<Self> {
        Ok(Self {
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

    pub fn blob_gas_price_next_block(&self) -> u128 {
        get_blob_gasprice(calc_excess_blob_gas(
            self.excess_blob_gas,
            self.blob_gas_used,
        ))
    }

    pub fn blob_gas_price(&self) -> u128 {
        // TODO: cache
        get_blob_gasprice(self.excess_blob_gas)
    }
}

fn calc_excess_blob_gas(parent_excess_blob_gas: u128, parent_blob_gas_used: u128) -> u128 {
    if parent_excess_blob_gas + parent_blob_gas_used < TARGET_BLOB_GAS_PER_BLOCK {
        0
    } else {
        parent_excess_blob_gas + parent_blob_gas_used - TARGET_BLOB_GAS_PER_BLOCK
    }
}

/// All transactions in a block must satisfy that
/// assert tx.max_fee_per_blob_gas >= get_blob_gasprice(block.header)
pub fn get_blob_gasprice(excess_blob_gas: u128) -> u128 {
    fake_exponential(
        MIN_BLOB_GASPRICE,
        excess_blob_gas,
        BLOB_GASPRICE_UPDATE_FRACTION,
    )
}

fn fake_exponential(factor: u128, numerator: u128, denominator: u128) -> u128 {
    let mut i = 1;
    let mut output = 0;
    let mut numerator_accum = factor * denominator;

    while numerator_accum > 0 {
        output += numerator_accum;
        numerator_accum = (numerator_accum * numerator) / (denominator * i);
        i += 1;
    }

    output / denominator
}

impl From<BlockWithTxs> for BlockGasSummary {
    fn from(value: BlockWithTxs) -> Self {
        Self {
            blob_gas_used: value.blob_gas_used,
            excess_blob_gas: value.excess_blob_gas,
            base_fee_per_gas: value.base_fee_per_gas,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calc_excess_blob_gas() {
        assert_eq!(calc_excess_blob_gas(TARGET_BLOB_GAS_PER_BLOCK, 0), 0);
        assert_eq!(calc_excess_blob_gas(0, TARGET_BLOB_GAS_PER_BLOCK), 0);
        assert_eq!(
            calc_excess_blob_gas(2 * TARGET_BLOB_GAS_PER_BLOCK, 0),
            TARGET_BLOB_GAS_PER_BLOCK
        );
    }

    #[test]
    fn test_get_blob_gasprice() {
        for (excess_blob_gas, expected_gas_price) in [
            (0, 1),
            (TARGET_BLOB_GAS_PER_BLOCK, 1),
            (2 * TARGET_BLOB_GAS_PER_BLOCK, 1),
            (BLOB_GASPRICE_UPDATE_FRACTION, 2),
            (2 * BLOB_GASPRICE_UPDATE_FRACTION, 7),
            (10 * BLOB_GASPRICE_UPDATE_FRACTION, 22026),
            (20 * BLOB_GASPRICE_UPDATE_FRACTION, 485165195),
            (30 * BLOB_GASPRICE_UPDATE_FRACTION, 10686474581524),
        ] {
            assert_eq!(
                get_blob_gasprice(excess_blob_gas),
                expected_gas_price,
                "({}, {})",
                excess_blob_gas,
                expected_gas_price
            );
        }
    }

    #[test]
    fn test_fake_exponential() {
        assert_eq!(fake_exponential(2, 3, 2), 8);
        assert_eq!(fake_exponential(2, 0, 2), 2);
        assert_eq!(fake_exponential(1, 50, 1), 5184612586559446279969);
        assert_eq!(
            fake_exponential(1, 50 * 1000000, 1000000),
            5184705528494131044804
        );
    }
}
