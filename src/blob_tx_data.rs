use ethers::types::{Address, Transaction, H256};
use eyre::{bail, eyre, Result};

pub const BLOB_TX_TYPE: u8 = 0x03;
const PARTICIPANT_DATA_SIZE: usize = 28;

#[derive(Clone, Debug)]
pub struct BlobTxParticipant {
    pub participant: Address,
    pub data_len: u64,
}

#[derive(Clone, Debug)]
pub struct BlobTxSummary {
    pub participants: Vec<BlobTxParticipant>,
    pub wei_per_byte: u128,
    pub tx_hash: H256,
    pub from: Address,
    pub nonce: u64,
}

impl BlobTxSummary {
    pub fn cost_to_participant(&self, address: Address) -> u128 {
        let mut cost: u128 = 0;

        for participant in &self.participants {
            if participant.participant == address {
                cost += participant.data_len as u128 * self.wei_per_byte;
            }
        }

        cost
    }

    pub fn from_tx(tx: Transaction) -> Result<Option<BlobTxSummary>> {
        if tx.transaction_type.map(|x| x.as_u64() as u8) != Some(BLOB_TX_TYPE) {
            return Ok(None);
        }

        let version = tx
            .input
            .get(0)
            .ok_or_else(|| eyre!("input must include a version"))?;

        if *version != 0 {
            bail!("only version 0 support");
        }

        let mut participants = vec![];
        let mut ptr = 1;

        while ptr + PARTICIPANT_DATA_SIZE <= tx.input.len() {
            let data_len = u64::from_be_bytes(tx.input[ptr..ptr + 8].try_into()?);
            ptr += 8;

            let mut address = [0u8; 20];
            address.copy_from_slice(&tx.input[ptr..ptr + 20]);
            ptr += 20;

            participants.push(BlobTxParticipant {
                participant: address.into(),
                data_len,
            });
        }

        // TODO: update transaction types to include price per byte
        let wei_per_byte = tx.gas_price.ok_or_else(|| eyre!("gas price not present"))?;

        Ok(Some(BlobTxSummary {
            participants,
            wei_per_byte: wei_per_byte.as_u128(),
            tx_hash: tx.hash,
            from: tx.from,
            nonce: tx.nonce.as_u64(),
        }))
    }
}

pub fn encode_blob_tx_data(participants: &[BlobTxParticipant]) -> Vec<u8> {
    let mut out = vec![0_u8];
    for participant in participants {
        out.extend_from_slice(&participant.data_len.to_be_bytes());
        out.extend_from_slice(participant.participant.as_bytes());
    }
    out
}
