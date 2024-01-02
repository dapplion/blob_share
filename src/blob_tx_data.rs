use std::io::{Cursor, Read, Write};

use c_kzg::BYTES_PER_BLOB;
use ethers::types::{Address, Transaction, H256};
use eyre::{bail, eyre, Context, Result};

use crate::{gas::GasConfig, utils::get_max_fee_per_blob_gas, BlockGasSummary};

pub const BLOB_TX_TYPE: u8 = 0x03;
const PARTICIPANT_DATA_SIZE: usize = 20 + 4;

#[derive(Clone, Debug, PartialEq)]
pub struct BlobTxSummary {
    pub participants: Vec<BlobTxParticipant>,
    pub tx_hash: H256,
    pub from: Address,
    pub nonce: u64,
    pub gas: GasConfig,
    pub used_bytes: usize,
}

impl BlobTxSummary {
    /// Returns true if this gas config is underpriced against a block gas summary
    pub fn is_underpriced(&self, block_gas: &BlockGasSummary) -> bool {
        self.gas.is_underpriced(block_gas)
    }

    pub fn cost_to_intent(&self, data_len: usize, block_gas: Option<BlockGasSummary>) -> u128 {
        let blob_gas_price = match block_gas {
            Some(block_gas) => block_gas.blob_gas_price(),
            None => self.gas.max_fee_per_blob_gas,
        };
        let base_fee_per_gas = match block_gas {
            Some(block_gas) => self.effective_gas_price(Some(block_gas.base_fee_per_gas)),
            None => self.effective_gas_price(None),
        };

        let unused_bytes = BYTES_PER_BLOB.saturating_sub(self.used_bytes);
        // Max product here is half blob unsued, half used by a single participant. Max
        // blob size is 2**17, so the max intermediary value is 2**16 * 2**16 = 2**32
        let attributable_unused_data =
            (unused_bytes as u128 * data_len as u128) / self.used_bytes as u128;

        let blob_data_cost = (attributable_unused_data + data_len as u128) * blob_gas_price;
        let evm_gas_cost = BlobTxParticipant::evm_gas() as u128 * base_fee_per_gas;

        blob_data_cost + evm_gas_cost
    }

    pub fn cost_to_participant(
        &self,
        address: &Address,
        block_gas: Option<BlockGasSummary>,
    ) -> u128 {
        let mut cost: u128 = 0;

        for participant in &self.participants {
            if &participant.address == address {
                cost += self.cost_to_intent(participant.data_len, block_gas)
            }
        }

        cost
    }

    fn effective_gas_price(&self, block_base_fee_per_gas: Option<u128>) -> u128 {
        if let Some(block_base_fee_per_gas) = block_base_fee_per_gas {
            self.gas.max_priority_fee_per_gas + block_base_fee_per_gas
        } else {
            self.gas.max_fee_per_gas
        }
    }

    pub fn participation_count_from(&self, from: &Address) -> usize {
        self.participants
            .iter()
            .filter(|participant| &participant.address == from)
            .count()
    }

    pub fn from_tx(tx: &Transaction) -> Result<Option<BlobTxSummary>> {
        if !is_blob_tx(tx) {
            return Ok(None);
        }

        let mut r = Cursor::new(&tx.input);

        let mut participants = vec![];

        while r.position() < tx.input.len() as u64 {
            participants
                .push(BlobTxParticipant::read(&mut r).wrap_err("invalid participant format")?);
        }

        // From EIP-1559
        // ```
        // priority_fee_per_gas = min(transaction.max_priority_fee_per_gas, transaction.max_fee_per_gas - block.base_fee_per_gas)
        // signer pays both the priority fee and the base fee
        // effective_gas_price = priority_fee_per_gas + block.base_fee_per_gas
        // signer.balance -= transaction.gas_limit * effective_gas_price
        // ```
        let max_fee_per_gas = tx
            .max_fee_per_gas
            .ok_or_else(|| eyre!("not a type 2 tx, no max_fee_per_gas"))?
            .as_u128();
        let max_priority_fee_per_gas = tx
            .max_priority_fee_per_gas
            .ok_or_else(|| eyre!("not a type 2 tx, no max_priority_fee_per_gas"))?
            .as_u128();
        let max_fee_per_blob_gas = get_max_fee_per_blob_gas(tx)?;

        let used_bytes = participants.iter().map(|p| p.data_len).sum::<usize>();

        Ok(Some(BlobTxSummary {
            participants,
            tx_hash: tx.hash,
            from: tx.from,
            nonce: tx.nonce.as_u64(),
            used_bytes,
            gas: GasConfig {
                max_priority_fee_per_gas,
                max_fee_per_gas,
                max_fee_per_blob_gas,
            },
        }))
    }
}

impl Into<GasConfig> for &BlobTxSummary {
    fn into(self) -> GasConfig {
        self.gas
    }
}

const PARTICIPANT_VERSION_1: u8 = 0x01;

#[derive(Clone, Debug, PartialEq)]
pub struct BlobTxParticipant {
    pub address: Address,
    // Max blob size is 2**17, a u32 can represent all possible data_len values
    pub data_len: usize,
}

impl BlobTxParticipant {
    fn evm_gas() -> usize {
        // TODO: make generic on different types of attributability
        16 * PARTICIPANT_DATA_SIZE
    }

    fn write<W: Write>(&self, w: &mut W) -> Result<(), std::io::Error> {
        w.write_all(&[PARTICIPANT_VERSION_1])?;
        w.write_all(&(self.data_len as u32).to_be_bytes())?;
        w.write_all(self.address.as_bytes())?;
        Ok(())
    }

    fn read<R: Read>(r: &mut R) -> Result<Self> {
        let mut version = [0; 1];
        r.read_exact(&mut version)?;

        if version[0] != PARTICIPANT_VERSION_1 {
            bail!("invalid participant version {}", version[0]);
        }

        let mut data_len = [0; 4];
        r.read_exact(&mut data_len)?;
        let mut address = [0; 20];
        r.read_exact(&mut address)?;
        Ok(Self {
            address: address.into(),
            data_len: u32::from_be_bytes(data_len) as usize,
        })
    }
}

pub fn encode_blob_tx_data(participants: &[BlobTxParticipant]) -> Result<Vec<u8>, std::io::Error> {
    let mut out = vec![0_u8];
    let mut w = Cursor::new(&mut out);
    for participant in participants {
        participant.write(&mut w)?;
    }
    Ok(out)
}

pub fn is_blob_tx(tx: &Transaction) -> bool {
    tx.transaction_type.map(|x| x.as_u64() as u8) == Some(BLOB_TX_TYPE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers::types::Transaction;

    fn generate_blob_tx_summary(participants: &[(u8, usize)]) -> BlobTxSummary {
        BlobTxSummary {
            participants: participants
                .iter()
                .map(|(addr, data_len)| BlobTxParticipant {
                    address: gen_addr(*addr),
                    data_len: *data_len,
                })
                .collect(),
            tx_hash: H256::default(),
            from: Address::default(),
            nonce: 0,
            used_bytes: participants.iter().map(|(_, data_len)| data_len).sum(),
            max_priority_fee_per_gas: 1,
            max_fee_per_gas: 1,
            max_fee_per_blob_gas: 1,
        }
    }

    fn gen_addr(b: u8) -> Address {
        [b; 20].into()
    }

    fn test_cost_to_participant(address: u8, participants: &[(u8, usize)], expected_cost: usize) {
        assert_eq!(
            generate_blob_tx_summary(&participants).cost_to_participant(
                &gen_addr(address),
                None,
                None
            ),
            expected_cost as u128
        );
    }

    #[test]
    fn test_cost_to_participant_no_participants() {
        test_cost_to_participant(1, &[], 0);
    }

    #[test]
    fn test_cost_to_participant_no_match() {
        test_cost_to_participant(1, &[(2, 10)], 0);
    }

    #[test]
    fn test_cost_to_participant_single_match() {
        test_cost_to_participant(
            1,
            &[(2, 3 * BYTES_PER_BLOB / 4), (1, BYTES_PER_BLOB / 4)],
            BYTES_PER_BLOB / 4 + 16 * 24, // blob + evm gas
        );
    }

    #[test]
    fn test_cost_to_participant_multiple_match() {
        test_cost_to_participant(
            1,
            &[
                (2, 6 * BYTES_PER_BLOB / 16),
                (1, BYTES_PER_BLOB / 16),
                (3, 7 * BYTES_PER_BLOB / 16),
                (1, 2 * BYTES_PER_BLOB / 16),
            ],
            3 * BYTES_PER_BLOB / 16 + 2 * 16 * 24, // blob + 2 * evm gas
        );
    }

    #[test]
    fn test_cost_to_participant_account_for_unused_bytes() {
        test_cost_to_participant(
            1,
            &[(2, 2 * BYTES_PER_BLOB / 4), (1, BYTES_PER_BLOB / 4)],
            // Unused data portion
            (BYTES_PER_BLOB / 4) / 3 +
            // Actual data portion
            BYTES_PER_BLOB / 4 + 16 * 24, // blob + 2 * evm gas
        );
    }

    #[test]
    fn block_tx_participants_serde() {
        let participants = (1..4)
            .map(|i| BlobTxParticipant {
                address: [10 + i; 20].into(),
                data_len: 10000 * i as usize,
            })
            .collect::<Vec<_>>();
        let used_bytes = (1..4).sum::<usize>() * 10000;

        let input = encode_blob_tx_data(&participants).unwrap();
        let nonce = 1234;
        let tx_hash = H256([0xdc; 32]);

        assert_eq!(hex::encode(&input), "01000027100b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0100004e200c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c01000075300d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d");

        let target_address: Address = [0xab; 20].into();
        let mut tx = Transaction::default();
        tx.transaction_type = Some(BLOB_TX_TYPE.into());
        tx.hash = tx_hash;
        tx.from = target_address;
        tx.nonce = nonce.into();
        tx.input = input.into();
        tx.max_fee_per_gas = Some(1.into());
        tx.max_priority_fee_per_gas = Some(2.into());
        tx.other.insert("maxFeePerBlobGas".to_string(), "3".into());

        let blob_tx_summary = BlobTxSummary::from_tx(&tx).unwrap().unwrap();
        let expected_blob_tx_summary = BlobTxSummary {
            participants,
            tx_hash,
            from: target_address,
            nonce,
            used_bytes,
            max_fee_per_gas: 1,
            max_priority_fee_per_gas: 2,
            max_fee_per_blob_gas: 3,
        };

        assert_eq!(blob_tx_summary, expected_blob_tx_summary);
    }
}
