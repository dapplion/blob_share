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
        self.data_len() as u128 * self.max_blob_gas_price() as u128
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

    pub fn data_len(&self) -> usize {
        // TODO: charge and coerce data to at least 31 bytes to prevent too expensive packing
        // rounds. Add consistency tests for signed data of coerced inputs
        self.data().len()
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
