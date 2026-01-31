use std::cmp::Ordering;

use alloy_primitives::{Address, B256, U256};
use c_kzg::{BYTES_PER_BLOB, FIELD_ELEMENTS_PER_BLOB};
use ethers::{
    signers::{LocalWallet, Signer},
    types::{Bytes, H256},
    utils::keccak256,
};
use eyre::{bail, Result};
use reth_primitives::Signature;
use sha2::{Digest, Sha256};

use crate::{
    blob_tx_data::{encode_blob_tx_data, BlobTxParticipant, BlobTxSummary},
    gas::GasConfig,
    reth_fork::{
        tx_eip4844::TxEip4844,
        tx_sidecar::{BlobTransaction, BlobTransactionSidecar},
    },
    MAX_USABLE_BLOB_DATA_LEN,
};

pub const VERSIONED_HASH_VERSION_KZG: u8 = 0x01;

pub struct BlobTx {
    #[allow(dead_code)]
    pub blob_tx_payload_body: Bytes,
    pub tx_hash: H256,
    pub blob_tx_networking: Bytes,
    pub tx_summary: BlobTxSummary,
}

pub(crate) struct TxParams {
    pub(crate) nonce: u64,
    pub(crate) chain_id: u64,
}

pub fn kzg_to_versioned_hash(commitment: &c_kzg::KzgCommitment) -> B256 {
    let mut res = Sha256::digest(commitment.as_slice());
    res[0] = VERSIONED_HASH_VERSION_KZG;
    B256::new(res.into())
}

pub(crate) fn construct_blob_tx(
    kzg_settings: &c_kzg::KzgSettings,
    l1_inbox_address: ethers::types::Address,
    gas_config: &GasConfig,
    tx_params: &TxParams,
    wallet: &LocalWallet,
    participants: Vec<BlobTxParticipant>,
    datas: Vec<Vec<u8>>,
) -> Result<BlobTx> {
    let mut data = vec![];
    for item in datas.into_iter() {
        // TODO: do less copying
        data.extend_from_slice(&item);
    }

    // Pad data to fit blob. Field element safety (each element < BLS_MODULUS) is handled
    // by encode_data_to_blob which chunks data into 31-byte segments with a leading zero byte.
    let target_data_len = FIELD_ELEMENTS_PER_BLOB * 31;
    match data.len().cmp(&target_data_len) {
        Ordering::Less => data.extend(vec![0; target_data_len - data.len()]),
        Ordering::Equal => {}
        Ordering::Greater => bail!("data longer than blob capacity"),
    }

    let blob = c_kzg::Blob::from_bytes(&encode_data_to_blob(&data))?;
    let commitment = c_kzg::KzgCommitment::blob_to_kzg_commitment(&blob, kzg_settings)?;
    let versioned_hash = kzg_to_versioned_hash(&commitment);
    let proof =
        c_kzg::KzgProof::compute_blob_kzg_proof(&blob, &commitment.to_bytes(), kzg_settings)?
            .to_bytes();

    // TODO customize by participant request on include signature or not
    let input = encode_blob_tx_data(&participants)?;
    let gas_limit = estimate_gas_limit(&input);

    let tx = TxEip4844 {
        chain_id: tx_params.chain_id,
        nonce: tx_params.nonce,
        max_priority_fee_per_gas: gas_config.max_priority_fee_per_gas,
        max_fee_per_gas: gas_config.max_fee_per_gas,
        gas_limit,
        to: Address::from(l1_inbox_address.to_fixed_bytes()),
        value: <_>::default(),
        input: input.into(),
        access_list: <_>::default(),
        max_fee_per_blob_gas: gas_config.max_fee_per_blob_gas,
        blob_versioned_hashes: vec![versioned_hash],
    };

    let sigature_hash = tx.signature_hash();

    let signature = wallet.sign_hash(H256::from_slice(sigature_hash.as_ref()))?;
    // Convert Signature from ethers-rs to alloy
    let signature = Signature {
        r: U256::from_limbs(signature.r.0),
        s: U256::from_limbs(signature.s.0),
        odd_y_parity: signature.v - 27 != 0,
    };

    let (tx_hash, tx_rlp_with_sig) = compute_blob_tx_hash(&tx, &signature);

    let blob_tx = BlobTransaction {
        hash: tx_hash.into(),
        transaction: tx,
        signature,
        sidecar: BlobTransactionSidecar {
            blobs: vec![blob],
            commitments: vec![commitment.to_bytes()],
            proofs: vec![proof],
        },
    };

    // The inner encoding is used with `with_header` set to true, making the final
    // encoding:
    // `rlp(tx_type || rlp([transaction_payload_body, blobs, commitments, proofs]))`
    let mut tx_rlp_networking = Vec::new();
    blob_tx.encode_with_type_inner(&mut tx_rlp_networking, false);

    let used_bytes = participants.iter().map(|p| p.data_len).sum();

    Ok(BlobTx {
        blob_tx_payload_body: tx_rlp_with_sig.into(),
        tx_hash: H256(tx_hash),
        blob_tx_networking: tx_rlp_networking.into(),
        tx_summary: BlobTxSummary {
            participants,
            tx_hash: H256::from_slice(tx_hash.as_ref()),
            from: wallet.address(),
            nonce: tx_params.nonce,
            used_bytes,
            gas: *gas_config,
        },
    })
}

// # Calculating the hash
//
// The full encoding of the `PooledTransaction` response is:
// `tx_type (0x03) || rlp([tx_payload_body, blobs, commitments, proofs])`
//
// The transaction hash however, is:
// `keccak256(tx_type (0x03) || rlp(tx_payload_body))`
//
// Note that this is `tx_payload_body`, not `[tx_payload_body]`, which would be
// `[[chain_id, nonce, max_priority_fee_per_gas, ...]]`, i.e. a list within a list.
//
// Because the pooled transaction encoding is different than the hash encoding for
// EIP-4844 transactions, we do not use the original buffer to calculate the hash.
//
// Instead, we use `encode_with_signature`, which RLP encodes the transaction with a
// signature for hashing without a header. We then hash the result.
pub fn compute_blob_tx_hash(tx: &TxEip4844, signature: &Signature) -> ([u8; 32], Vec<u8>) {
    let mut tx_rlp_with_sig = Vec::new();
    tx.encode_with_signature(signature, &mut tx_rlp_with_sig, false);
    (keccak256(&tx_rlp_with_sig), tx_rlp_with_sig)
}

/// Base EVM gas cost for a transaction (EIP-2028 intrinsic gas).
const TX_BASE_GAS: u64 = 21_000;
/// EVM calldata cost per zero byte.
const CALLDATA_GAS_PER_ZERO_BYTE: u64 = 4;
/// EVM calldata cost per non-zero byte.
const CALLDATA_GAS_PER_NONZERO_BYTE: u64 = 16;
/// Safety margin added to gas estimate to account for EVM execution overhead.
const GAS_LIMIT_SAFETY_MARGIN: u64 = 5_000;

/// Estimate the EVM gas limit for a blob transaction based on its calldata (input).
///
/// Gas = 21,000 (base) + calldata_cost + safety_margin
///
/// Calldata cost: 4 gas per zero byte, 16 gas per non-zero byte (EIP-2028).
pub(crate) fn estimate_gas_limit(input: &[u8]) -> u64 {
    let calldata_gas: u64 = input
        .iter()
        .map(|&b| {
            if b == 0 {
                CALLDATA_GAS_PER_ZERO_BYTE
            } else {
                CALLDATA_GAS_PER_NONZERO_BYTE
            }
        })
        .sum();

    TX_BASE_GAS + calldata_gas + GAS_LIMIT_SAFETY_MARGIN
}

/// Encode raw data into blob format by chunking into 31-byte field elements.
///
/// Each 32-byte field element in the blob has a leading zero byte followed by 31 bytes of data.
/// The zero high byte ensures the field element value stays below BLS_MODULUS, which is required
/// for valid KZG commitments and proofs.
///
/// Reference: <https://github.com/ethpandaops/goomy-blob/blob/e4b460b17b6e2748995ef3d7b75cbe967dc49da4/txbuilder/blob_encode.go#L36>
fn encode_data_to_blob(data: &[u8]) -> Vec<u8> {
    let mut chunked_blob_data = vec![0u8; BYTES_PER_BLOB];
    for (field_index, chunk) in data.chunks(31).enumerate() {
        chunked_blob_data[field_index * 32 + 1..field_index * 32 + 1 + chunk.len()]
            .copy_from_slice(chunk);
    }
    chunked_blob_data
}

pub(crate) fn decode_blob_to_data(blob: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(MAX_USABLE_BLOB_DATA_LEN);
    for chunk in blob.chunks(32) {
        data.extend_from_slice(&chunk[1..32]);
    }
    data
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use ethers::{
        signers::{LocalWallet, Signer},
        types::Address,
    };
    use eyre::Result;

    use crate::{
        blob_tx_data::BlobTxParticipant,
        gas::GasConfig,
        kzg::{decode_blob_to_data, TxParams},
        load_kzg_settings,
        reth_fork::tx_sidecar::BlobTransaction,
        ADDRESS_ZERO, MAX_USABLE_BLOB_DATA_LEN,
    };

    use super::{
        construct_blob_tx, encode_data_to_blob, estimate_gas_limit, CALLDATA_GAS_PER_NONZERO_BYTE,
        CALLDATA_GAS_PER_ZERO_BYTE, GAS_LIMIT_SAFETY_MARGIN, TX_BASE_GAS,
    };

    const DEV_PRIVKEY: &str = "392a230386a19b84b6b865067d5493b158e987d28104ab16365854a8fd851bb0";

    #[test]
    fn encode_decode_blob_data() {
        let data = vec![0xaa; MAX_USABLE_BLOB_DATA_LEN];
        let blob = encode_data_to_blob(&data);
        assert_eq!(hex::encode(&decode_blob_to_data(&blob)), hex::encode(&data));
    }

    #[test]
    fn encode_data_to_blob_high_byte_is_zero() {
        // Verify each 32-byte field element has a zero leading byte,
        // ensuring the value is < BLS_MODULUS.
        let data = vec![0xff; MAX_USABLE_BLOB_DATA_LEN];
        let blob = encode_data_to_blob(&data);
        for (i, chunk) in blob.chunks(32).enumerate() {
            assert_eq!(
                chunk[0], 0,
                "field element {} has non-zero high byte: 0x{:02x}",
                i, chunk[0]
            );
        }
    }

    #[test]
    fn encode_decode_small_data() {
        // Data smaller than 31 bytes fits in a single field element
        let data = vec![0x42; 10];
        let blob = encode_data_to_blob(&data);
        let decoded = decode_blob_to_data(&blob);
        assert_eq!(&decoded[..10], &data[..]);
    }

    #[test]
    fn encode_decode_exactly_31_bytes() {
        // Exactly one field element worth of data
        let data = vec![0xab; 31];
        let blob = encode_data_to_blob(&data);
        let decoded = decode_blob_to_data(&blob);
        assert_eq!(&decoded[..31], &data[..]);
    }

    #[test]
    fn encode_decode_crosses_field_element_boundary() {
        // 32 bytes spans two field elements (31 + 1)
        let data: Vec<u8> = (0..32).collect();
        let blob = encode_data_to_blob(&data);
        let decoded = decode_blob_to_data(&blob);
        assert_eq!(&decoded[..32], &data[..]);
    }

    #[test]
    fn encode_data_to_blob_empty_input() {
        let blob = encode_data_to_blob(&[]);
        // All bytes should be zero
        assert!(blob.iter().all(|&b| b == 0));
        assert_eq!(blob.len(), c_kzg::BYTES_PER_BLOB);
    }

    #[test]
    fn kzg_to_versioned_hash_sets_version_byte() {
        use super::kzg_to_versioned_hash;
        let commitment = c_kzg::KzgCommitment::from([0u8; 48]);
        let hash = kzg_to_versioned_hash(&commitment);
        assert_eq!(hash[0], super::VERSIONED_HASH_VERSION_KZG);
    }

    #[tokio::test]
    async fn test_construct_blob_tx() -> Result<()> {
        let chain_id = 999;
        let wallet = LocalWallet::from_bytes(&hex::decode(DEV_PRIVKEY)?)?.with_chain_id(chain_id);
        let gas_config = GasConfig {
            max_fee_per_gas: 1u128.into(),
            max_fee_per_blob_gas: 1u128.into(),
            max_priority_fee_per_gas: 1u128.into(),
        };

        let mut participants: Vec<BlobTxParticipant> = vec![];
        let mut datas: Vec<Vec<u8>> = vec![];
        for i in 0..2 {
            let wallet = LocalWallet::from_bytes(&[i + 1; 32])?;
            let data = vec![i + 0x10; 1000 * i as usize];
            participants.push(BlobTxParticipant {
                address: wallet.address(),
                data_len: data.len(),
            });
            datas.push(data);
        }

        let blob_tx = construct_blob_tx(
            &load_kzg_settings()?,
            Address::from_str(ADDRESS_ZERO)?,
            &gas_config,
            &TxParams { chain_id, nonce: 0 },
            &wallet,
            participants.clone(),
            datas,
        )?;

        // EIP-2718 TransactionPayload
        assert_eq!(blob_tx.blob_tx_payload_body[0], 0x03);

        // Networking transaction is also prefixed by 0x03
        assert_eq!(blob_tx.blob_tx_networking[0], 0x03);
        let mut blob_tx_networking = &blob_tx.blob_tx_networking[1..];
        let decoded_tx = BlobTransaction::decode_inner(&mut blob_tx_networking)?;

        let recovered_address = decoded_tx
            .signature
            .recover_signer(decoded_tx.transaction.signature_hash())
            .expect("bad signature");
        assert_eq!(
            Address::from_slice(&recovered_address.0 .0),
            wallet.address()
        );

        // Assert gas
        assert_eq!(blob_tx.tx_summary.gas, gas_config);

        // Assert participants
        assert_eq!(blob_tx.tx_summary.participants, participants);

        Ok(())
    }

    #[test]
    fn estimate_gas_limit_empty_input() {
        // Empty input: only base gas + safety margin
        assert_eq!(
            estimate_gas_limit(&[]),
            TX_BASE_GAS + GAS_LIMIT_SAFETY_MARGIN
        );
    }

    #[test]
    fn estimate_gas_limit_all_zero_bytes() {
        let input = vec![0u8; 10];
        assert_eq!(
            estimate_gas_limit(&input),
            TX_BASE_GAS + 10 * CALLDATA_GAS_PER_ZERO_BYTE + GAS_LIMIT_SAFETY_MARGIN
        );
    }

    #[test]
    fn estimate_gas_limit_all_nonzero_bytes() {
        let input = vec![0xffu8; 10];
        assert_eq!(
            estimate_gas_limit(&input),
            TX_BASE_GAS + 10 * CALLDATA_GAS_PER_NONZERO_BYTE + GAS_LIMIT_SAFETY_MARGIN
        );
    }

    #[test]
    fn estimate_gas_limit_mixed_bytes() {
        // 3 zero bytes + 7 non-zero bytes
        let mut input = vec![0u8; 3];
        input.extend_from_slice(&[0x01; 7]);
        assert_eq!(
            estimate_gas_limit(&input),
            TX_BASE_GAS
                + 3 * CALLDATA_GAS_PER_ZERO_BYTE
                + 7 * CALLDATA_GAS_PER_NONZERO_BYTE
                + GAS_LIMIT_SAFETY_MARGIN
        );
    }

    #[test]
    fn estimate_gas_limit_realistic_participant_data() {
        // Simulate encoded blob tx data for 3 participants.
        // Each participant is 25 bytes: 1 version (0x01) + 4 data_len (BE) + 20 address.
        use crate::blob_tx_data::encode_blob_tx_data;

        let participants = (1..=3)
            .map(|i| BlobTxParticipant {
                address: [i; 20].into(),
                data_len: 1000 * i as usize,
            })
            .collect::<Vec<_>>();
        let input = encode_blob_tx_data(&participants).unwrap();

        // 3 participants * 25 bytes = 75 bytes
        assert_eq!(input.len(), 75);

        // Compute expected gas by counting actual zero/non-zero bytes
        let zero_count = input.iter().filter(|&&b| b == 0).count() as u64;
        let nonzero_count = input.len() as u64 - zero_count;
        let expected_gas = TX_BASE_GAS
            + zero_count * CALLDATA_GAS_PER_ZERO_BYTE
            + nonzero_count * CALLDATA_GAS_PER_NONZERO_BYTE
            + GAS_LIMIT_SAFETY_MARGIN;
        assert_eq!(estimate_gas_limit(&input), expected_gas);

        // Gas limit should be well under 100k for 3 participants
        assert!(expected_gas < 30_000);
    }

    #[tokio::test]
    async fn construct_blob_tx_uses_dynamic_gas_limit() -> Result<()> {
        use crate::blob_tx_data::encode_blob_tx_data;

        let chain_id = 999;
        let wallet = LocalWallet::from_bytes(&hex::decode(DEV_PRIVKEY)?)?.with_chain_id(chain_id);
        let gas_config = GasConfig {
            max_fee_per_gas: 1u128,
            max_fee_per_blob_gas: 1u128,
            max_priority_fee_per_gas: 1u128,
        };

        // 1 participant
        let participant_wallet = LocalWallet::from_bytes(&[1; 32])?;
        let participants = vec![BlobTxParticipant {
            address: participant_wallet.address(),
            data_len: 500,
        }];
        let datas = vec![vec![0xaa; 500]];

        let blob_tx = construct_blob_tx(
            &load_kzg_settings()?,
            Address::from_str(ADDRESS_ZERO)?,
            &gas_config,
            &TxParams { chain_id, nonce: 0 },
            &wallet,
            participants.clone(),
            datas,
        )?;

        // Decode the tx and verify gas_limit matches estimate_gas_limit
        let mut blob_tx_networking = &blob_tx.blob_tx_networking[1..];
        let decoded_tx = BlobTransaction::decode_inner(&mut blob_tx_networking)?;

        let expected_input = encode_blob_tx_data(&participants)?;
        let expected_gas_limit = estimate_gas_limit(&expected_input);
        assert_eq!(decoded_tx.transaction.gas_limit, expected_gas_limit);

        // Gas limit should be well below the old hardcoded 100k for a single participant
        assert!(
            expected_gas_limit < 100_000,
            "gas limit {} should be less than 100k for 1 participant",
            expected_gas_limit
        );

        Ok(())
    }
}
