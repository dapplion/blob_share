use std::{
    fmt::{self, Display},
    str::FromStr,
};

use ethers::{
    signers::{LocalWallet, Signer},
    types::{Address, Signature},
    utils::keccak256,
};
use eyre::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_utils::hex_vec;

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
    pub max_blob_gas_price: u128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DataIntentWithSignature {
    pub from: Address,
    pub data: Vec<u8>,
    pub data_hash: DataHash,
    pub max_blob_gas_price: u128,
    pub signature: Signature,
}

impl DataIntent {
    pub fn max_cost(&self) -> u128 {
        self.data().len() as u128 * self.max_blob_gas_price()
    }

    pub fn max_blob_gas_price(&self) -> u128 {
        match self {
            DataIntent::NoSignature(d) => d.max_blob_gas_price,
            DataIntent::WithSignature(d) => d.max_blob_gas_price,
        }
    }

    pub fn id(&self) -> DataIntentId {
        match self {
            DataIntent::NoSignature(d) => DataIntentId::new(d.from, d.data_hash),
            DataIntent::WithSignature(d) => DataIntentId::new(d.from, d.data_hash),
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

    pub async fn with_signature(
        wallet: &LocalWallet,
        data: Vec<u8>,
        max_blob_gas_price: u128,
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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DataIntentSummary {
    pub id: String,
    pub from: Address,
    #[serde(with = "hex_vec")]
    pub data_hash: Vec<u8>,
    pub data_len: usize,
    pub max_blob_gas_price: u128,
}

impl From<&DataIntent> for DataIntentSummary {
    fn from(value: &DataIntent) -> Self {
        let id = value.id().to_string();
        match value {
            DataIntent::WithSignature(d) => Self {
                id,
                from: d.from,
                data_hash: d.data_hash.to_vec(),
                data_len: d.data.len(),
                max_blob_gas_price: d.max_blob_gas_price,
            },
            DataIntent::NoSignature(d) => Self {
                id,
                from: d.from,
                data_hash: d.data_hash.to_vec(),
                data_len: d.data.len(),
                max_blob_gas_price: d.max_blob_gas_price,
            },
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct DataIntentId(Address, DataHash);

impl DataIntentId {
    fn new(from: Address, data_hash: DataHash) -> Self {
        Self(from, data_hash)
    }
}

impl std::fmt::Debug for DataIntentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v1-{}-{}", hex::encode(self.0), hex::encode(self.1 .0))
    }
}

impl Display for DataIntentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v1-{}-{}", hex::encode(self.0), hex::encode(self.1 .0))
    }
}

impl FromStr for DataIntentId {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split('-').collect();

        if parts.len() != 3 {
            bail!("Invalid id format format".to_string());
        }
        let version = parts[0];
        let address = parts[1];
        let data_hash = parts[2];

        if version != "v1" {
            bail!("Unsupported version {}", version);
        }

        let address = Address::from_str(address)?;
        let data_hash = DataHash::from_str(data_hash)?;

        Ok(DataIntentId::new(address, data_hash))
    }
}

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
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use ethers::types::H160;

    use super::*;

    #[test]
    fn data_intent_id_str_serde() {
        let id = DataIntentId::new(H160([0xab; 20]), [0xfe; 32].into());
        let id_str = id.to_string();
        assert_eq!(id_str, "v1-abababababababababababababababababababab-fefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefe");
        assert_eq!(DataIntentId::from_str(&id_str).unwrap(), id);
    }
}
