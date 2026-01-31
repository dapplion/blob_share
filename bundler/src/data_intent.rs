use bundler_client::types::DataHash;
use ethers::{
    signers::{LocalWallet, Signer},
    types::{Address, Signature},
};
use eyre::Result;
use serde::{Deserialize, Serialize};

// Max gas price possible to represent is ~18 ETH / byte, or ~2.4e6 ETH per blob
pub type BlobGasPrice = u64;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum DataIntent {
    NoSignature(DataIntentNoSignature),
    WithSignature(DataIntentWithSignature),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DataIntentNoSignature {
    pub from: Address,
    pub data: Vec<u8>,
    pub data_hash: DataHash,
    pub max_blob_gas_price: BlobGasPrice,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DataIntentWithSignature {
    pub from: Address,
    pub data: Vec<u8>,
    pub data_hash: DataHash,
    pub max_blob_gas_price: BlobGasPrice,
    pub signature: Signature,
}

impl DataIntent {
    pub fn max_cost(&self) -> u128 {
        self.chargeable_data_len() as u128 * self.max_blob_gas_price() as u128
    }

    pub fn max_blob_gas_price(&self) -> BlobGasPrice {
        match self {
            DataIntent::NoSignature(d) => d.max_blob_gas_price,
            DataIntent::WithSignature(d) => d.max_blob_gas_price,
        }
    }

    pub fn from(&self) -> &Address {
        match self {
            DataIntent::NoSignature(d) => &d.from,
            DataIntent::WithSignature(d) => &d.from,
        }
    }

    /// Returns the actual byte length of the data, used for packing and capacity checks.
    pub fn data_len(&self) -> usize {
        self.data().len()
    }

    /// Returns the data length used for cost calculation, floored to one field element (31 bytes).
    pub fn chargeable_data_len(&self) -> usize {
        self.data()
            .len()
            .max(bundler_client::types::MIN_CHARGEABLE_DATA_LEN)
    }

    pub fn data(&self) -> &[u8] {
        match self {
            DataIntent::NoSignature(d) => &d.data,
            DataIntent::WithSignature(d) => &d.data,
        }
    }

    pub fn data_hash(&self) -> &DataHash {
        match self {
            DataIntent::NoSignature(d) => &d.data_hash,
            DataIntent::WithSignature(d) => &d.data_hash,
        }
    }

    pub fn data_hash_signature(&self) -> Option<&Signature> {
        match self {
            DataIntent::NoSignature(_) => None,
            DataIntent::WithSignature(d) => Some(&d.signature),
        }
    }

    pub async fn with_signature(
        wallet: &LocalWallet,
        data: Vec<u8>,
        max_blob_gas_price: BlobGasPrice,
    ) -> Result<Self> {
        let data_hash = DataHash::from_data(&data);
        let signature: Signature = wallet.sign_message(data_hash.to_vec()).await?;

        Ok(Self::WithSignature(DataIntentWithSignature {
            from: wallet.address(),
            data,
            data_hash,
            signature,
            max_blob_gas_price,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_intent(data: Vec<u8>, max_blob_gas_price: BlobGasPrice) -> DataIntent {
        DataIntent::NoSignature(DataIntentNoSignature {
            from: Address::zero(),
            data_hash: DataHash::from_data(&data),
            data,
            max_blob_gas_price,
        })
    }

    #[test]
    fn data_len_returns_raw_length() {
        let intent = make_intent(vec![0x42], 100);
        assert_eq!(intent.data_len(), 1);
    }

    #[test]
    fn chargeable_data_len_enforces_minimum() {
        // 1 byte of data should be charged as 31 bytes
        let intent = make_intent(vec![0x42], 100);
        assert_eq!(intent.chargeable_data_len(), 31);
    }

    #[test]
    fn chargeable_data_len_at_minimum_boundary() {
        // Exactly 31 bytes should remain 31
        let intent = make_intent(vec![0u8; 31], 100);
        assert_eq!(intent.chargeable_data_len(), 31);
    }

    #[test]
    fn chargeable_data_len_above_minimum() {
        // 100 bytes should remain 100
        let intent = make_intent(vec![0u8; 100], 100);
        assert_eq!(intent.chargeable_data_len(), 100);
    }

    #[test]
    fn max_cost_uses_chargeable_data_len() {
        let intent = make_intent(vec![0x42], 100);
        // Cost should be 31 * 100 = 3100, not 1 * 100
        assert_eq!(intent.max_cost(), 31 * 100);
    }

    #[test]
    fn max_cost_large_data_unchanged() {
        let intent = make_intent(vec![0u8; 500], 200);
        assert_eq!(intent.max_cost(), 500 * 200);
    }
}
