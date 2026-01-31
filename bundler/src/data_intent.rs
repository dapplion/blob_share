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

    // -- Accessor methods on NoSignature variant --

    #[test]
    fn from_returns_address_no_signature() {
        let addr = Address::from([0xAB; 20]);
        let intent = DataIntent::NoSignature(DataIntentNoSignature {
            from: addr,
            data: vec![1, 2, 3],
            data_hash: DataHash::from_data(&[1, 2, 3]),
            max_blob_gas_price: 50,
        });
        assert_eq!(*intent.from(), addr);
    }

    #[test]
    fn data_returns_slice_no_signature() {
        let intent = make_intent(vec![0xAA, 0xBB, 0xCC], 10);
        assert_eq!(intent.data(), &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn data_hash_matches_keccak_no_signature() {
        let data = vec![1, 2, 3, 4, 5];
        let expected_hash = DataHash::from_data(&data);
        let intent = make_intent(data, 10);
        assert_eq!(*intent.data_hash(), expected_hash);
    }

    #[test]
    fn data_hash_signature_none_for_no_signature() {
        let intent = make_intent(vec![0x42], 100);
        assert!(intent.data_hash_signature().is_none());
    }

    #[test]
    fn max_blob_gas_price_no_signature() {
        let intent = make_intent(vec![0x42], 12345);
        assert_eq!(intent.max_blob_gas_price(), 12345);
    }

    // -- WithSignature variant --

    fn make_signed_intent(data: Vec<u8>, max_blob_gas_price: BlobGasPrice) -> DataIntent {
        // Use a fixed signature for deterministic tests
        let sig = Signature {
            r: ethers::types::U256::from(1u64),
            s: ethers::types::U256::from(2u64),
            v: 27,
        };
        let addr = Address::from([0xBB; 20]);
        DataIntent::WithSignature(DataIntentWithSignature {
            from: addr,
            data_hash: DataHash::from_data(&data),
            data,
            max_blob_gas_price,
            signature: sig,
        })
    }

    #[test]
    fn from_returns_address_with_signature() {
        let intent = make_signed_intent(vec![1, 2], 50);
        assert_eq!(*intent.from(), Address::from([0xBB; 20]));
    }

    #[test]
    fn data_returns_slice_with_signature() {
        let intent = make_signed_intent(vec![0xDE, 0xAD], 10);
        assert_eq!(intent.data(), &[0xDE, 0xAD]);
    }

    #[test]
    fn data_hash_matches_keccak_with_signature() {
        let data = vec![10, 20, 30];
        let expected_hash = DataHash::from_data(&data);
        let intent = make_signed_intent(data, 10);
        assert_eq!(*intent.data_hash(), expected_hash);
    }

    #[test]
    fn data_hash_signature_some_for_with_signature() {
        let intent = make_signed_intent(vec![0x42], 100);
        let sig = intent.data_hash_signature();
        assert!(sig.is_some());
        assert_eq!(sig.unwrap().r, ethers::types::U256::from(1u64));
        assert_eq!(sig.unwrap().s, ethers::types::U256::from(2u64));
    }

    #[test]
    fn max_blob_gas_price_with_signature() {
        let intent = make_signed_intent(vec![0x42], 99999);
        assert_eq!(intent.max_blob_gas_price(), 99999);
    }

    #[test]
    fn data_len_with_signature() {
        let intent = make_signed_intent(vec![0u8; 64], 10);
        assert_eq!(intent.data_len(), 64);
    }

    #[test]
    fn chargeable_data_len_with_signature_enforces_minimum() {
        let intent = make_signed_intent(vec![0x42], 100);
        assert_eq!(intent.chargeable_data_len(), 31);
    }

    #[test]
    fn max_cost_with_signature_uses_chargeable_len() {
        let intent = make_signed_intent(vec![0x42], 100);
        // 1 byte data → charged as 31 bytes → cost = 31 * 100
        assert_eq!(intent.max_cost(), 31 * 100);
    }

    // -- with_signature constructor --

    #[tokio::test]
    async fn with_signature_creates_valid_intent() {
        let wallet = LocalWallet::from_bytes(&[0xAC; 32]).unwrap();
        let data = vec![1, 2, 3, 4, 5];
        let intent = DataIntent::with_signature(&wallet, data.clone(), 500)
            .await
            .unwrap();

        assert_eq!(*intent.from(), wallet.address());
        assert_eq!(intent.data(), &data);
        assert_eq!(intent.max_blob_gas_price(), 500);
        assert_eq!(*intent.data_hash(), DataHash::from_data(&data));
        assert!(intent.data_hash_signature().is_some());
    }

    #[tokio::test]
    async fn with_signature_is_with_signature_variant() {
        let wallet = LocalWallet::from_bytes(&[0xAC; 32]).unwrap();
        let intent = DataIntent::with_signature(&wallet, vec![0xFF], 1)
            .await
            .unwrap();
        assert!(matches!(intent, DataIntent::WithSignature(_)));
    }

    // -- Serde roundtrips --

    #[test]
    fn serde_roundtrip_no_signature() {
        let intent = make_intent(vec![1, 2, 3], 42);
        let json = serde_json::to_string(&intent).unwrap();
        let deserialized: DataIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, intent);
    }

    #[test]
    fn serde_roundtrip_with_signature() {
        let intent = make_signed_intent(vec![4, 5, 6], 77);
        let json = serde_json::to_string(&intent).unwrap();
        let deserialized: DataIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, intent);
    }

    // -- Edge cases --

    #[test]
    fn empty_data_intent() {
        let intent = make_intent(vec![], 100);
        assert_eq!(intent.data_len(), 0);
        // Empty data still charged at minimum 31 bytes
        assert_eq!(intent.chargeable_data_len(), 31);
        assert_eq!(intent.max_cost(), 31 * 100);
    }

    #[test]
    fn zero_gas_price_intent() {
        let intent = make_intent(vec![0u8; 100], 0);
        assert_eq!(intent.max_cost(), 0);
        assert_eq!(intent.max_blob_gas_price(), 0);
    }

    #[test]
    fn max_gas_price_no_overflow() {
        // u64::MAX gas price with 31-byte minimum should not overflow u128
        let intent = make_intent(vec![0x42], u64::MAX);
        let expected = 31u128 * u64::MAX as u128;
        assert_eq!(intent.max_cost(), expected);
    }
}
