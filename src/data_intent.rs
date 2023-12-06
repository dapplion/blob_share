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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DataIntent {
    pub from: Address,
    pub data: Vec<u8>,
    pub data_hash: DataHash,
    pub signature: Signature,
    pub max_cost_wei: u128,
}

impl DataIntent {
    pub fn verify_signature(&self) -> Result<()> {
        Ok(self.signature.verify(self.message_to_sign(), self.from)?)
    }

    pub fn message_to_sign(&self) -> &[u8] {
        // TODO: must sign over max_cost_wei too
        // Use RLP to serialize multiple fields together
        &self.data_hash.0
    }

    pub fn id(&self) -> DataIntentId {
        DataIntentId::new(self.from, self.data_hash)
    }

    pub async fn with_signature(
        wallet: &LocalWallet,
        data: Vec<u8>,
        max_cost_wei: u128,
    ) -> Result<Self> {
        let data_hash = DataHash::from_data(&data);
        let signature: Signature = wallet.sign_message(data_hash.0).await?;

        Ok(DataIntent {
            from: wallet.address(),
            data,
            data_hash,
            signature,
            max_cost_wei,
        })
    }
}

pub fn deserialize_signature(signature: &[u8]) -> Result<Signature> {
    Ok(signature.try_into()?)
}

#[derive(Clone, Copy, Serialize, Deserialize, Hash, Eq, PartialEq, Debug)]
pub struct DataIntentId(Address, DataHash);

impl DataIntentId {
    fn new(from: Address, data_hash: DataHash) -> Self {
        Self(from, data_hash)
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

#[derive(Clone, Copy, Serialize, Deserialize, Hash, Eq, PartialEq, Debug)]
pub struct DataHash([u8; 32]);

impl Display for DataHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format!("0x{}", hex::encode(self.0)))
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
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use ethers::{
        signers::{LocalWallet, Signer},
        types::{Address, H160},
    };
    use eyre::Result;

    use super::{DataIntent, DataIntentId};

    #[test]
    fn data_intent_id_str_serde() {
        let id = DataIntentId::new(H160([0xab; 20]), [0xfe; 32].into());
        let id_str = id.to_string();
        assert_eq!(id_str, "v1-abababababababababababababababababababab-fefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefe");
        assert_eq!(DataIntentId::from_str(&id_str).unwrap(), id);
    }

    #[tokio::test]
    async fn data_intent_signature() -> Result<()> {
        let data = vec![0xaa; 50];
        let max_cost_wei = 100000;
        let wallet = get_wallet()?;

        let data_intent = DataIntent::with_signature(&wallet, data, max_cost_wei).await?;

        data_intent.verify_signature()?;
        Ok(())
    }

    const DEV_PRIVKEY: &str = "392a230386a19b84b6b865067d5493b158e987d28104ab16365854a8fd851bb0";
    const DEV_PUBKEY: &str = "0xdbD48e742FF3Ecd3Cb2D557956f541b6669b3277";

    pub fn get_wallet() -> Result<LocalWallet> {
        let wallet = LocalWallet::from_bytes(&hex::decode(DEV_PRIVKEY)?)?;
        assert_eq!(wallet.address(), Address::from_str(DEV_PUBKEY)?);
        Ok(wallet)
    }
}
