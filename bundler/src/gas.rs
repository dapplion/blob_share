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
    pub fn reprice_to_at_least(&mut self, other_gas: GasConfig) {
        self.max_priority_fee_per_gas = cmp::max(
            self.max_priority_fee_per_gas,
            (11 * other_gas.max_priority_fee_per_gas) / 10,
        );
        self.max_fee_per_gas = cmp::max(
            self.max_fee_per_gas, //
            (11 * other_gas.max_fee_per_gas) / 10,
        );
        self.max_fee_per_blob_gas = cmp::max(
            self.max_fee_per_blob_gas,
            (11 * other_gas.max_fee_per_blob_gas) / 10,
        );
    }

    /// Returns true if this gas config is underpriced against a block gas summary.
    ///
    /// Checks three conditions:
    /// 1. max_fee_per_gas must cover the block's base fee
    /// 2. max_fee_per_blob_gas must cover the block's blob gas price
    /// 3. max_fee_per_gas must leave room for the priority fee (i.e.
    ///    max_fee_per_gas >= base_fee + max_priority_fee_per_gas)
    pub fn is_underpriced(&self, block_gas: &BlockGasSummary) -> bool {
        // EVM gas underpriced: max_fee doesn't cover base fee
        block_gas.base_fee_per_gas > self.max_fee_per_gas
        // Blob gas underpriced
            || block_gas.blob_gas_price() > self.max_fee_per_blob_gas
        // Priority fee underpriced: max_fee can't cover base_fee + priority_fee.
        // Miners/validators won't include a tx where the effective priority fee
        // is zero because max_fee - base_fee < priority_fee.
            || self.max_fee_per_gas < block_gas.base_fee_per_gas + self.max_priority_fee_per_gas
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to build a BlockGasSummary with specific base_fee and excess_blob_gas.
    /// `blob_gas_used` is set to 0 since blob_gas_price() only depends on excess_blob_gas.
    fn block_gas(base_fee: u128, excess_blob_gas: u128) -> BlockGasSummary {
        BlockGasSummary {
            base_fee_per_gas: base_fee,
            excess_blob_gas,
            blob_gas_used: 0,
        }
    }

    #[test]
    fn is_underpriced_returns_false_when_all_fees_sufficient() {
        let gas = GasConfig {
            max_priority_fee_per_gas: 2_000_000_000,
            max_fee_per_gas: 30_000_000_000,
            max_fee_per_blob_gas: 100,
        };
        // base_fee 10 gwei, low blob gas (excess 0 → blob price 1)
        let bg = block_gas(10_000_000_000, 0);
        assert!(!gas.is_underpriced(&bg));
    }

    #[test]
    fn is_underpriced_returns_true_when_base_fee_exceeds_max_fee() {
        let gas = GasConfig {
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 10_000_000_000,
            max_fee_per_blob_gas: 100,
        };
        // base_fee higher than max_fee_per_gas
        let bg = block_gas(20_000_000_000, 0);
        assert!(gas.is_underpriced(&bg));
    }

    #[test]
    fn is_underpriced_returns_true_when_blob_gas_too_high() {
        let gas = GasConfig {
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 30_000_000_000,
            max_fee_per_blob_gas: 1, // very low blob gas budget
        };
        // High excess_blob_gas → high blob gas price
        // excess = 10 * BLOB_GASPRICE_UPDATE_FRACTION → blob price = 22026
        let bg = block_gas(10_000_000_000, 10 * 3338477);
        assert!(gas.is_underpriced(&bg));
    }

    #[test]
    fn is_underpriced_returns_true_when_priority_fee_cannot_be_covered() {
        // max_fee barely covers base_fee, no room for priority fee
        let gas = GasConfig {
            max_priority_fee_per_gas: 5_000_000_000,
            max_fee_per_gas: 10_000_000_000, // equals base_fee
            max_fee_per_blob_gas: 100,
        };
        let bg = block_gas(10_000_000_000, 0);
        // max_fee (10) < base_fee (10) + priority_fee (5) → underpriced
        assert!(gas.is_underpriced(&bg));
    }

    #[test]
    fn is_underpriced_priority_fee_exactly_fits() {
        // max_fee = base_fee + priority_fee exactly
        let gas = GasConfig {
            max_priority_fee_per_gas: 2_000_000_000,
            max_fee_per_gas: 12_000_000_000,
            max_fee_per_blob_gas: 100,
        };
        let bg = block_gas(10_000_000_000, 0);
        // max_fee (12) >= base_fee (10) + priority_fee (2) → not underpriced
        assert!(!gas.is_underpriced(&bg));
    }

    #[test]
    fn is_underpriced_with_zero_priority_fee() {
        let gas = GasConfig {
            max_priority_fee_per_gas: 0,
            max_fee_per_gas: 10_000_000_000,
            max_fee_per_blob_gas: 100,
        };
        let bg = block_gas(10_000_000_000, 0);
        // max_fee (10) >= base_fee (10) + priority_fee (0) → not underpriced
        assert!(!gas.is_underpriced(&bg));
    }

    #[test]
    fn reprice_to_at_least_bumps_all_fields() {
        let mut gas = GasConfig {
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 10_000_000_000,
            max_fee_per_blob_gas: 100,
        };
        let other = GasConfig {
            max_priority_fee_per_gas: 2_000_000_000,
            max_fee_per_gas: 20_000_000_000,
            max_fee_per_blob_gas: 200,
        };
        gas.reprice_to_at_least(other);
        // Should be at least 110% of other
        assert!(gas.max_priority_fee_per_gas >= 2_200_000_000);
        assert!(gas.max_fee_per_gas >= 22_000_000_000);
        assert!(gas.max_fee_per_blob_gas >= 220);
    }

    #[test]
    fn reprice_to_at_least_keeps_higher_values() {
        let mut gas = GasConfig {
            max_priority_fee_per_gas: 10_000_000_000,
            max_fee_per_gas: 100_000_000_000,
            max_fee_per_blob_gas: 1000,
        };
        let other = GasConfig {
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 10_000_000_000,
            max_fee_per_blob_gas: 100,
        };
        gas.reprice_to_at_least(other);
        // Original values are higher, should be kept
        assert_eq!(gas.max_priority_fee_per_gas, 10_000_000_000);
        assert_eq!(gas.max_fee_per_gas, 100_000_000_000);
        assert_eq!(gas.max_fee_per_blob_gas, 1000);
    }
}
