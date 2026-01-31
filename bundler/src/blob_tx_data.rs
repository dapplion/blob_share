use std::io::{Cursor, Read, Write};

use bundler_client::types::BlockGasSummary;
use c_kzg::BYTES_PER_BLOB;
use ethers::types::{Address, Transaction, H256};
use eyre::{bail, eyre, Context, Result};

use crate::{gas::GasConfig, utils::get_max_fee_per_blob_gas};

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
            gas: GasConfig {
                max_priority_fee_per_gas: 1,
                max_fee_per_gas: 1,
                max_fee_per_blob_gas: 1,
            },
        }
    }

    fn gen_addr(b: u8) -> Address {
        [b; 20].into()
    }

    fn test_cost_to_participant(address: u8, participants: &[(u8, usize)], expected_cost: usize) {
        assert_eq!(
            generate_blob_tx_summary(&participants).cost_to_participant(&gen_addr(address), None,),
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
            gas: GasConfig {
                max_fee_per_gas: 1,
                max_priority_fee_per_gas: 2,
                max_fee_per_blob_gas: 3,
            },
        };

        assert_eq!(blob_tx_summary, expected_blob_tx_summary);
    }

    // --- is_blob_tx ---

    #[test]
    fn is_blob_tx_true_for_type_3() {
        let mut tx = Transaction::default();
        tx.transaction_type = Some(BLOB_TX_TYPE.into());
        assert!(is_blob_tx(&tx));
    }

    #[test]
    fn is_blob_tx_false_for_type_2() {
        let mut tx = Transaction::default();
        tx.transaction_type = Some(2.into());
        assert!(!is_blob_tx(&tx));
    }

    #[test]
    fn is_blob_tx_false_for_none_type() {
        let tx = Transaction::default();
        assert!(!is_blob_tx(&tx));
    }

    // --- encode_blob_tx_data ---

    #[test]
    fn encode_blob_tx_data_empty_participants() {
        // Encoding with no participants should return only the leading zero byte
        let data = encode_blob_tx_data(&[]).unwrap();
        assert_eq!(data, vec![0u8]);
    }

    #[test]
    fn encode_blob_tx_data_single_participant() {
        let participant = BlobTxParticipant {
            address: gen_addr(0xAB),
            data_len: 256,
        };
        let data = encode_blob_tx_data(&[participant]).unwrap();
        // Cursor starts at position 0 in the vec, overwriting the initial zero.
        // version(1) + data_len(4) + address(20) = 25 bytes per participant
        assert_eq!(data.len(), 25);
        // Version byte at position 0
        assert_eq!(data[0], PARTICIPANT_VERSION_1);
        // data_len as big-endian u32: 256 = 0x00000100
        assert_eq!(&data[1..5], &[0x00, 0x00, 0x01, 0x00]);
    }

    // --- BlobTxParticipant::read ---

    #[test]
    fn participant_read_invalid_version() {
        let mut buf = vec![0x02]; // Invalid version
        buf.extend_from_slice(&[0; 24]); // Enough bytes for data_len + address
        let result = BlobTxParticipant::read(&mut &buf[..]);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid participant version"),
            "should report invalid version"
        );
    }

    #[test]
    fn participant_read_truncated_input() {
        // Only version byte, no data_len or address
        let buf = vec![PARTICIPANT_VERSION_1];
        let result = BlobTxParticipant::read(&mut &buf[..]);
        assert!(result.is_err());
    }

    // --- BlobTxSummary::from_tx ---

    #[test]
    fn from_tx_non_blob_returns_none() {
        let mut tx = Transaction::default();
        tx.transaction_type = Some(2.into());
        let result = BlobTxSummary::from_tx(&tx).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn from_tx_missing_max_fee_per_gas() {
        let participants = vec![BlobTxParticipant {
            address: gen_addr(0xBB),
            data_len: 1000,
        }];
        let input = encode_blob_tx_data(&participants).unwrap();
        let mut tx = Transaction::default();
        tx.transaction_type = Some(BLOB_TX_TYPE.into());
        tx.input = input.into();
        // max_fee_per_gas is None
        tx.max_priority_fee_per_gas = Some(1.into());
        tx.other.insert("maxFeePerBlobGas".to_string(), "1".into());

        let result = BlobTxSummary::from_tx(&tx);
        assert!(result.is_err());
    }

    #[test]
    fn from_tx_missing_max_priority_fee() {
        let participants = vec![BlobTxParticipant {
            address: gen_addr(0xBB),
            data_len: 1000,
        }];
        let input = encode_blob_tx_data(&participants).unwrap();
        let mut tx = Transaction::default();
        tx.transaction_type = Some(BLOB_TX_TYPE.into());
        tx.input = input.into();
        tx.max_fee_per_gas = Some(1.into());
        // max_priority_fee_per_gas is None
        tx.other.insert("maxFeePerBlobGas".to_string(), "1".into());

        let result = BlobTxSummary::from_tx(&tx);
        assert!(result.is_err());
    }

    #[test]
    fn from_tx_missing_max_fee_per_blob_gas() {
        let participants = vec![BlobTxParticipant {
            address: gen_addr(0xBB),
            data_len: 1000,
        }];
        let input = encode_blob_tx_data(&participants).unwrap();
        let mut tx = Transaction::default();
        tx.transaction_type = Some(BLOB_TX_TYPE.into());
        tx.input = input.into();
        tx.max_fee_per_gas = Some(1.into());
        tx.max_priority_fee_per_gas = Some(1.into());
        // maxFeePerBlobGas not in `other`

        let result = BlobTxSummary::from_tx(&tx);
        assert!(result.is_err());
    }

    #[test]
    fn from_tx_empty_input() {
        // Blob tx with empty input (no participants)
        let mut tx = Transaction::default();
        tx.transaction_type = Some(BLOB_TX_TYPE.into());
        tx.input = vec![].into();
        tx.max_fee_per_gas = Some(1.into());
        tx.max_priority_fee_per_gas = Some(1.into());
        tx.other.insert("maxFeePerBlobGas".to_string(), "1".into());

        let result = BlobTxSummary::from_tx(&tx).unwrap().unwrap();
        assert!(result.participants.is_empty());
        assert_eq!(result.used_bytes, 0);
    }

    // --- participation_count_from ---

    #[test]
    fn participation_count_from_no_match() {
        let summary = generate_blob_tx_summary(&[(2, 1000), (3, 2000)]);
        assert_eq!(summary.participation_count_from(&gen_addr(1)), 0);
    }

    #[test]
    fn participation_count_from_single_match() {
        let summary = generate_blob_tx_summary(&[(1, 1000), (2, 2000)]);
        assert_eq!(summary.participation_count_from(&gen_addr(1)), 1);
    }

    #[test]
    fn participation_count_from_multiple_matches() {
        let summary = generate_blob_tx_summary(&[(1, 1000), (2, 2000), (1, 3000)]);
        assert_eq!(summary.participation_count_from(&gen_addr(1)), 2);
    }

    // --- cost_to_intent with block gas ---

    #[test]
    fn cost_to_intent_with_block_gas_uses_actual_prices() {
        let summary = generate_blob_tx_summary(&[(1, BYTES_PER_BLOB)]);
        let block_gas = BlockGasSummary {
            base_fee_per_gas: 10,
            excess_blob_gas: 0, // blob_gas_price() == 1
            blob_gas_used: 0,
        };

        let cost_with_block = summary.cost_to_intent(BYTES_PER_BLOB, Some(block_gas));
        let cost_without_block = summary.cost_to_intent(BYTES_PER_BLOB, None);

        // With block gas: blob_gas_price=1 (from excess=0), effective_gas_price=priority(1)+base(10)=11
        // Without block gas: blob_gas_price=max_fee_per_blob_gas(1), effective_gas_price=max_fee_per_gas(1)
        // The EVM gas component differs: 11 * evm_gas vs 1 * evm_gas
        assert!(
            cost_with_block > cost_without_block,
            "cost with base_fee=10 ({cost_with_block}) should exceed cost with max_fee=1 ({cost_without_block})"
        );
    }

    #[test]
    fn cost_to_intent_full_blob_no_unused_space() {
        // When used_bytes == BYTES_PER_BLOB, attributable_unused_data = 0
        let summary = generate_blob_tx_summary(&[(1, BYTES_PER_BLOB)]);
        let cost = summary.cost_to_intent(BYTES_PER_BLOB, None);
        // cost = data_len * blob_gas_price + evm_gas * effective_gas_price
        // = BYTES_PER_BLOB * 1 + (16 * 24) * 1
        let expected = BYTES_PER_BLOB as u128 + (16 * PARTICIPANT_DATA_SIZE) as u128;
        assert_eq!(cost, expected);
    }

    // --- effective_gas_price (tested indirectly through cost_to_intent) ---

    #[test]
    fn cost_reflects_effective_gas_with_and_without_block_base_fee() {
        let summary = BlobTxSummary {
            participants: vec![BlobTxParticipant {
                address: gen_addr(1),
                data_len: BYTES_PER_BLOB,
            }],
            tx_hash: H256::default(),
            from: Address::default(),
            nonce: 0,
            used_bytes: BYTES_PER_BLOB,
            gas: GasConfig {
                max_priority_fee_per_gas: 5,
                max_fee_per_gas: 100,
                max_fee_per_blob_gas: 1,
            },
        };

        let block_gas = BlockGasSummary {
            base_fee_per_gas: 20,
            excess_blob_gas: 0,
            blob_gas_used: 0,
        };

        // With block gas: effective_gas_price = priority(5) + base_fee(20) = 25
        let cost_with = summary.cost_to_intent(BYTES_PER_BLOB, Some(block_gas));
        // Without block gas: effective_gas_price = max_fee_per_gas(100)
        let cost_without = summary.cost_to_intent(BYTES_PER_BLOB, None);

        let evm_gas = BlobTxParticipant::evm_gas() as u128;
        // blob cost is identical in both (BYTES_PER_BLOB * 1)
        // EVM cost differs: 25 * evm_gas vs 100 * evm_gas
        assert_eq!(cost_with, BYTES_PER_BLOB as u128 + 25 * evm_gas);
        assert_eq!(cost_without, BYTES_PER_BLOB as u128 + 100 * evm_gas);
    }

    // --- is_underpriced delegation ---

    #[test]
    fn blob_tx_summary_is_underpriced_delegates_to_gas_config() {
        let summary = BlobTxSummary {
            participants: vec![],
            tx_hash: H256::default(),
            from: Address::default(),
            nonce: 0,
            used_bytes: 0,
            gas: GasConfig {
                max_priority_fee_per_gas: 1,
                max_fee_per_gas: 10,
                max_fee_per_blob_gas: 100,
            },
        };
        let block_gas = BlockGasSummary {
            base_fee_per_gas: 5,
            excess_blob_gas: 0,
            blob_gas_used: 0,
        };
        // max_fee(10) >= base(5) + priority(1) → not underpriced
        assert!(!summary.is_underpriced(&block_gas));

        let expensive_block = BlockGasSummary {
            base_fee_per_gas: 50,
            excess_blob_gas: 0,
            blob_gas_used: 0,
        };
        // max_fee(10) < base(50) → underpriced
        assert!(summary.is_underpriced(&expensive_block));
    }
}
