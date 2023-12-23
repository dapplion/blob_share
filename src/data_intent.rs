use std::{
    fmt::{self, Display},
    str::FromStr,
};

use ethers::{
    signers::{LocalWallet, Signer},
    types::{Address, Signature},
    utils::keccak256,
};
use eyre::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
        data_intent_max_cost(self.data_len(), self.max_blob_gas_price())
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
        let signature: Signature = wallet.sign_message(data_hash.0).await?;

        Ok(Self::WithSignature(DataIntentWithSignature {
            from: wallet.address(),
            data,
            data_hash,
            signature,
            max_blob_gas_price,
        }))
    }
}

/// Max possible cost of data intent, billed cost prior to inclusion
pub(crate) fn data_intent_max_cost(data_len: usize, max_blob_gas_price: BlobGasPrice) -> u128 {
    data_len as u128 * max_blob_gas_price as u128
}

pub type DataIntentId = Uuid;

#[derive(Clone, Copy, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct DataHash([u8; 32]);

impl Display for DataHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format!("0x{}", hex::encode(self.0)))
    }
}

impl fmt::Debug for DataHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DataHash({})", hex::encode(self.0))
    }
}

impl From<[u8; 32]> for DataHash {
    fn from(value: [u8; 32]) -> Self {
        DataHash(value)
    }
}

impl FromStr for DataHash {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self> {
        let v = hex::decode(s)?;
        Ok(DataHash(v.as_slice().try_into()?))
    }
}

impl DataHash {
    pub fn from_data(data: &[u8]) -> DataHash {
        keccak256(data).into()
    }

    pub fn to_vec(self) -> Vec<u8> {
        self.0.to_vec()
    }

    pub fn to_fixed_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn data_intent_id_str_serde() {
        let id_str = "c4f1bdd0-3331-4470-b427-28a2c514f483";
        let id = DataIntentId::from_str(id_str).unwrap();
        assert_eq!(format!("{}", id), id_str);
        assert_eq!(format!("{:?}", id), id_str);

        let id_as_json = format!("\"{}\"", id_str);
        let id_from_json = serde_json::from_str(&id_as_json).unwrap();
        assert_eq!(id, id_from_json);
        assert_eq!(serde_json::to_string(&id).unwrap(), id_as_json);
    }
}
