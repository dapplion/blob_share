use crate::{
    option_hex_vec,
    utils::{deserialize_signature, unix_timestamps_millis},
};
use chrono::{DateTime, Utc};
use ethers::{
    signers::{LocalWallet, Signer},
    types::{Address, Signature, TxHash, H256},
    utils::keccak256,
};
use eyre::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_utils::hex_vec;
use std::{
    fmt::{self, Display},
    str::FromStr,
};
use uuid::Uuid;

pub use crate::gas::{BlockGasSummary, GasRecommendation, GasResponse};

pub type DataIntentId = Uuid;

pub type BlobGasPrice = u64;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataResponse {
    pub id: DataIntentId,
    /// When the submitted data was split across multiple blobs, `group_id` identifies the group
    /// and `chunk_ids` lists all chunk intent IDs. For single-blob submissions these are `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<DataIntentId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_ids: Option<Vec<DataIntentId>>,
}

/// TODO: Expose a "login with Ethereum" function an expose the non-signed variant
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataIntentV1 {
    /// Address sending the data
    pub from: Address,
    /// Data to be posted
    #[serde(with = "hex_vec")]
    pub data: Vec<u8>,
    /// Max price user is willing to pay in wei
    pub max_blob_gas_price: BlobGasPrice,
}

/// PostDataIntent message for non authenticated channels
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PostDataIntentV1Signed {
    pub intent: PostDataIntentV1,
    /// DataIntent nonce, to allow replay protection. Each new intent must have a nonce higher than
    /// the last known nonce from this `from` sender. Re-pricings will be done with a different
    /// nonce. For simplicity just pick the current UNIX timestemp in miliseconds.
    ///
    /// u64::MAX is 18446744073709551616, able to represent unix timestamps in miliseconds way into
    /// the future.
    pub nonce: u64,
    /// Signature over := data | nonce | max_blob_gas_price
    #[serde(with = "hex_vec")]
    pub signature: Vec<u8>,
}

impl PostDataIntentV1Signed {
    pub async fn with_signature(
        wallet: &LocalWallet,
        intent: PostDataIntentV1,
        nonce: Option<u64>,
    ) -> Result<Self> {
        if wallet.address() != intent.from {
            bail!(
                "intent.from {} does not match wallet address {}",
                intent.from,
                wallet.address()
            );
        }

        let nonce = nonce.unwrap_or_else(unix_timestamps_millis);
        let signature: Signature = wallet.sign_message(Self::sign_hash(&intent, nonce)).await?;

        Ok(Self {
            intent,
            nonce,
            signature: signature.into(),
        })
    }

    fn sign_hash(intent: &PostDataIntentV1, nonce: u64) -> Vec<u8> {
        let data_hash = DataHash::from_data(&intent.data);

        // Concat: data_hash | nonce | max_blob_gas_price
        let mut signed_data = data_hash.to_vec();
        signed_data.extend_from_slice(&intent.max_blob_gas_price.to_be_bytes());
        signed_data.extend_from_slice(&nonce.to_be_bytes());

        signed_data
    }

    pub fn verify_signature(&self) -> Result<()> {
        let signature = deserialize_signature(&self.signature)?;
        let sign_hash = PostDataIntentV1Signed::sign_hash(&self.intent, self.nonce);
        signature.verify(sign_hash, self.intent.from)?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum DataIntentStatus {
    Unknown,
    Pending,
    Cancelled,
    InPendingTx { tx_hash: TxHash },
    InConfirmedTx { tx_hash: TxHash, block_hash: H256 },
}

impl DataIntentStatus {
    pub fn is_known(&self) -> bool {
        match self {
            DataIntentStatus::InConfirmedTx { .. }
            | DataIntentStatus::InPendingTx { .. }
            | DataIntentStatus::Pending
            | DataIntentStatus::Cancelled => true,
            DataIntentStatus::Unknown => false,
        }
    }

    pub fn is_in_tx(&self) -> Option<TxHash> {
        match self {
            DataIntentStatus::Unknown | DataIntentStatus::Pending | DataIntentStatus::Cancelled => {
                None
            }
            DataIntentStatus::InPendingTx { tx_hash, .. } => Some(*tx_hash),
            DataIntentStatus::InConfirmedTx { tx_hash, .. } => Some(*tx_hash),
        }
    }

    pub fn is_in_block(&self) -> Option<(H256, TxHash)> {
        match self {
            DataIntentStatus::Unknown
            | DataIntentStatus::Pending
            | DataIntentStatus::Cancelled
            | DataIntentStatus::InPendingTx { .. } => None,
            DataIntentStatus::InConfirmedTx {
                tx_hash,
                block_hash,
            } => Some((*tx_hash, *block_hash)),
        }
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

    pub fn to_fixed_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SenderDetails {
    pub addresses: Vec<Address>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SyncStatus {
    pub anchor_block: SyncStatusBlock,
    pub synced_head: SyncStatusBlock,
    pub node_head: SyncStatusBlock,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SyncStatusBlock {
    pub hash: H256,
    pub number: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DataIntentFull {
    pub id: Uuid,
    #[serde(with = "hex_vec")]
    pub eth_address: Vec<u8>,
    #[serde(with = "hex_vec")]
    pub data: Vec<u8>,
    pub data_len: u32,
    #[serde(with = "hex_vec")]
    pub data_hash: Vec<u8>,
    pub max_blob_gas_price: BlobGasPrice,
    #[serde(with = "option_hex_vec")]
    pub data_hash_signature: Option<Vec<u8>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DataIntentSummary {
    pub id: DataIntentId,
    pub from: Address,
    #[serde(with = "hex_vec")]
    pub data_hash: Vec<u8>,
    pub data_len: usize,
    pub max_blob_gas_price: BlobGasPrice,
    pub updated_at: DateTime<Utc>,
    /// Shared across all chunks from a single split submission. None for single-blob intents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<DataIntentId>,
}

impl DataIntentSummary {
    pub fn max_cost(&self) -> u128 {
        data_intent_max_cost(self.data_len, self.max_blob_gas_price)
    }
}

/// Minimum data length for cost calculation: one field element (31 bytes).
/// Prevents too-small intents from being undercharged.
pub const MIN_CHARGEABLE_DATA_LEN: usize = 31;

/// Max possible cost of data intent, billed cost prior to inclusion.
/// Applies minimum chargeable length floor of one field element (31 bytes).
pub(crate) fn data_intent_max_cost(data_len: usize, max_blob_gas_price: BlobGasPrice) -> u128 {
    let chargeable_len = data_len.max(MIN_CHARGEABLE_DATA_LEN);
    chargeable_len as u128 * max_blob_gas_price as u128
}

/// Signed request to cancel a data intent
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CancelDataIntentSigned {
    /// Address that owns the data intent
    pub from: Address,
    /// ID of the data intent to cancel
    pub id: DataIntentId,
    /// Signature over := id_bytes | "cancel"
    #[serde(with = "hex_vec")]
    pub signature: Vec<u8>,
}

impl CancelDataIntentSigned {
    pub async fn with_signature(
        wallet: &LocalWallet,
        from: Address,
        id: DataIntentId,
    ) -> Result<Self> {
        if wallet.address() != from {
            bail!(
                "from {} does not match wallet address {}",
                from,
                wallet.address()
            );
        }

        let sign_hash = Self::sign_hash(id);
        let signature: Signature = wallet.sign_message(sign_hash).await?;

        Ok(Self {
            from,
            id,
            signature: signature.into(),
        })
    }

    fn sign_hash(id: DataIntentId) -> Vec<u8> {
        let mut signed_data = id.as_bytes().to_vec();
        signed_data.extend_from_slice(b"cancel");
        signed_data
    }

    pub fn verify_signature(&self) -> Result<()> {
        let signature = deserialize_signature(&self.signature)?;
        let sign_hash = Self::sign_hash(self.id);
        signature.verify(sign_hash, self.from)?;
        Ok(())
    }
}

/// A single entry in the address history response.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryEntry {
    pub id: DataIntentId,
    pub data_len: u32,
    #[serde(with = "hex_vec")]
    pub data_hash: Vec<u8>,
    pub max_blob_gas_price: BlobGasPrice,
    pub status: HistoryEntryStatus,
    pub updated_at: DateTime<Utc>,
}

/// Status of a history entry, combining cancelled/finalized/inclusion info into one enum.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum HistoryEntryStatus {
    Pending,
    Cancelled,
    Included { tx_hash: TxHash },
    Finalized { tx_hash: TxHash },
}

/// Paginated response for address history.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryResponse {
    pub entries: Vec<HistoryEntry>,
    pub total: u64,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use ethers::signers::{LocalWallet, Signer};

    use super::*;

    const DEV_PRIVKEY: &str = "392a230386a19b84b6b865067d5493b158e987d28104ab16365854a8fd851bb0";

    fn get_wallet() -> Result<LocalWallet> {
        Ok(LocalWallet::from_bytes(&hex::decode(DEV_PRIVKEY)?)?)
    }

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

    #[tokio::test]
    async fn cancel_data_intent_signature_valid() -> Result<()> {
        let wallet = get_wallet()?;
        let id = DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483")?;

        let cancel = CancelDataIntentSigned::with_signature(&wallet, wallet.address(), id).await?;
        cancel.verify_signature()?;

        Ok(())
    }

    #[tokio::test]
    async fn cancel_data_intent_signature_wrong_address() -> Result<()> {
        let wallet = get_wallet()?;
        let id = DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483")?;

        let mut cancel =
            CancelDataIntentSigned::with_signature(&wallet, wallet.address(), id).await?;
        // Tamper with the from address
        cancel.from = Address::zero();

        assert!(cancel.verify_signature().is_err());

        Ok(())
    }

    #[tokio::test]
    async fn cancel_data_intent_wallet_mismatch() -> Result<()> {
        let wallet = get_wallet()?;
        let id = DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483")?;

        let result = CancelDataIntentSigned::with_signature(&wallet, Address::zero(), id).await;
        assert!(result.is_err());

        Ok(())
    }

    #[tokio::test]
    async fn cancel_data_intent_serde_roundtrip() -> Result<()> {
        let wallet = get_wallet()?;
        let id = DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483")?;

        let cancel = CancelDataIntentSigned::with_signature(&wallet, wallet.address(), id).await?;
        let json = serde_json::to_string(&cancel)?;
        let cancel_deserialized: CancelDataIntentSigned = serde_json::from_str(&json)?;

        assert_eq!(cancel.from, cancel_deserialized.from);
        assert_eq!(cancel.id, cancel_deserialized.id);
        assert_eq!(cancel.signature, cancel_deserialized.signature);
        cancel_deserialized.verify_signature()?;

        Ok(())
    }

    #[test]
    fn data_intent_status_cancelled_is_known() {
        assert!(DataIntentStatus::Cancelled.is_known());
        assert!(DataIntentStatus::Cancelled.is_in_tx().is_none());
        assert!(DataIntentStatus::Cancelled.is_in_block().is_none());
    }

    #[test]
    fn history_entry_pending_serde_roundtrip() {
        let entry = HistoryEntry {
            id: DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483").unwrap(),
            data_len: 1024,
            data_hash: vec![0xaa; 32],
            max_blob_gas_price: 5000,
            status: HistoryEntryStatus::Pending,
            updated_at: DateTime::from_str("2024-01-01T00:00:00Z").unwrap(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: HistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, entry.id);
        assert_eq!(deserialized.data_len, 1024);
        assert_eq!(deserialized.status, HistoryEntryStatus::Pending);
    }

    #[test]
    fn history_entry_included_serde_roundtrip() {
        let tx_hash = TxHash::zero();
        let entry = HistoryEntry {
            id: DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483").unwrap(),
            data_len: 512,
            data_hash: vec![0xbb; 32],
            max_blob_gas_price: 3000,
            status: HistoryEntryStatus::Included { tx_hash },
            updated_at: DateTime::from_str("2024-02-01T12:00:00Z").unwrap(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: HistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.status,
            HistoryEntryStatus::Included { tx_hash }
        );
    }

    #[test]
    fn history_entry_finalized_serde_roundtrip() {
        let tx_hash = TxHash::zero();
        let entry = HistoryEntry {
            id: DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483").unwrap(),
            data_len: 256,
            data_hash: vec![0xcc; 32],
            max_blob_gas_price: 1000,
            status: HistoryEntryStatus::Finalized { tx_hash },
            updated_at: DateTime::from_str("2024-03-01T00:00:00Z").unwrap(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: HistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.status,
            HistoryEntryStatus::Finalized { tx_hash }
        );
    }

    #[test]
    fn history_entry_cancelled_serde_roundtrip() {
        let entry = HistoryEntry {
            id: DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483").unwrap(),
            data_len: 64,
            data_hash: vec![0xdd; 32],
            max_blob_gas_price: 2000,
            status: HistoryEntryStatus::Cancelled,
            updated_at: DateTime::from_str("2024-04-01T00:00:00Z").unwrap(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: HistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.status, HistoryEntryStatus::Cancelled);
    }

    #[test]
    fn history_response_serde_roundtrip() {
        let response = HistoryResponse {
            entries: vec![HistoryEntry {
                id: DataIntentId::from_str("c4f1bdd0-3331-4470-b427-28a2c514f483").unwrap(),
                data_len: 100,
                data_hash: vec![0xee; 32],
                max_blob_gas_price: 500,
                status: HistoryEntryStatus::Pending,
                updated_at: DateTime::from_str("2024-05-01T00:00:00Z").unwrap(),
            }],
            total: 42,
        };
        let json = serde_json::to_string(&response).unwrap();
        let deserialized: HistoryResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total, 42);
        assert_eq!(deserialized.entries.len(), 1);
    }

    #[test]
    fn history_response_empty_entries() {
        let response = HistoryResponse {
            entries: vec![],
            total: 0,
        };
        let json = serde_json::to_string(&response).unwrap();
        let deserialized: HistoryResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total, 0);
        assert!(deserialized.entries.is_empty());
    }
}
