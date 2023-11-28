use ethers::providers::{Http, Middleware, Provider};
use eyre::Result;

pub struct GasConfig {
    pub max_priority_fee_per_gas: u128,
    pub max_fee_per_gas: u128,
    pub max_fee_per_blob_gas: u128,
}

impl GasConfig {
    pub async fn estimate(eth_provider: &Provider<Http>) -> Result<Self> {
        // Note: fetches latest block to estimate gas fees
        let (max_fee_per_gas, max_priority_fee_per_gas) =
            eth_provider.estimate_eip1559_fees(None).await?;

        // TODO: estimate blob gas
        let max_fee_per_blob_gas = 0;

        Ok(Self {
            max_priority_fee_per_gas: max_priority_fee_per_gas.as_u64().try_into()?,
            max_fee_per_gas: max_fee_per_gas.as_u64().try_into()?,
            max_fee_per_blob_gas: max_fee_per_blob_gas.try_into()?,
        })
    }
}
