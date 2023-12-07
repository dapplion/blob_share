use ethers::{
    providers::{Middleware, Provider, Ws},
    types::{Block, TxHash},
};
use eyre::{eyre, Result};
use tokio::sync::RwLock;

const MIN_BLOB_GASPRICE: u128 = 1;
const BLOB_GASPRICE_UPDATE_FRACTION: u128 = 3338477;
const TARGET_BLOB_GAS_PER_BLOCK: u128 = 393216;

pub struct GasConfig {
    pub max_priority_fee_per_gas: u128,
    pub max_fee_per_gas: u128,
    pub max_fee_per_blob_gas: u128,
}

#[derive(Clone)]
pub struct BlockGasSummary {
    blob_gas_used: u128,
    excess_blob_gas: u128,
}

pub struct GasTracker {
    current_head_gas: RwLock<BlockGasSummary>,
}

impl GasTracker {
    pub fn new(current_head: &Block<TxHash>) -> Result<Self> {
        Ok(Self {
            current_head_gas: BlockGasSummary::from_block(current_head)?.into(),
        })
    }
}

impl GasTracker {
    pub async fn new_head(&self, block: &Block<TxHash>) -> Result<()> {
        let gas_summary = BlockGasSummary::from_block(block)?;
        *self.current_head_gas.write().await = gas_summary;
        Ok(())
    }

    pub async fn estimate(&self, eth_provider: &Provider<Ws>) -> Result<GasConfig> {
        // Note: fetches latest block to estimate gas fees
        let (max_fee_per_gas, max_priority_fee_per_gas) =
            eth_provider.estimate_eip1559_fees(None).await?;

        let current_head_gas = self.current_head_gas.read().await.clone();
        let min_blob_gas_price_next_block = get_blob_gasprice(calc_excess_blob_gas(
            current_head_gas.excess_blob_gas,
            current_head_gas.blob_gas_used,
        ));

        let max_fee_per_blob_gas = (125 * min_blob_gas_price_next_block) / 100;

        Ok(GasConfig {
            max_priority_fee_per_gas: max_priority_fee_per_gas.as_u64().try_into()?,
            max_fee_per_gas: max_fee_per_gas.as_u64().try_into()?,
            max_fee_per_blob_gas,
        })
    }
}

impl BlockGasSummary {
    fn from_block(block: &Block<TxHash>) -> Result<Self> {
        Ok(Self {
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
pub(crate) fn get_blob_gasprice(excess_blob_gas: u128) -> u128 {
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

#[cfg(test)]
mod tests {
    use super::*;

    const TARGET_BLOB_GAS_PER_BLOCK: u128 = 393216;

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
