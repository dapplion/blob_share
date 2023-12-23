use std::cmp::Ordering;

use alloy_primitives::{keccak256, Address, B256, U256};
use c_kzg::{BYTES_PER_BLOB, FIELD_ELEMENTS_PER_BLOB};
use ethers::{
    signers::{LocalWallet, Signer},
    types::{Bytes, H256},
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
    PublishConfig, MAX_USABLE_BLOB_DATA_LEN,
};

pub const VERSIONED_HASH_VERSION_KZG: u8 = 0x01;

pub struct BlobTx {
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
    publish_config: &PublishConfig,
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

    // TODO: should chunk data in 31 bytes to ensure each field element if < BLS_MODULUS
    // pad data to fit blob
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

    let tx = TxEip4844 {
        chain_id: tx_params.chain_id,
        nonce: tx_params.nonce,
        max_priority_fee_per_gas: gas_config.max_priority_fee_per_gas,
        max_fee_per_gas: gas_config.max_fee_per_gas,
        // TODO Adjust gas with input
        gas_limit: 100_000_u64,
        to: Address::from(publish_config.l1_inbox_address.to_fixed_bytes()),
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
    let mut tx_rlp_with_sig = Vec::new();
    tx.encode_with_signature(&signature, &mut tx_rlp_with_sig, false);
    let tx_hash = keccak256(&tx_rlp_with_sig);

    let blob_tx = BlobTransaction {
        hash: tx_hash,
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
        tx_hash: H256(tx_hash.into()),
        blob_tx_networking: tx_rlp_networking.into(),
        tx_summary: BlobTxSummary {
            participants,
            tx_hash: H256::from_slice(tx_hash.as_ref()),
            from: wallet.address(),
            nonce: tx_params.nonce,
            used_bytes,
            max_priority_fee_per_gas: gas_config.max_priority_fee_per_gas,
            max_fee_per_gas: gas_config.max_fee_per_gas,
            max_fee_per_blob_gas: gas_config.max_fee_per_blob_gas,
        },
    })
}

// Chunk data in 31 bytes to ensure each field element is < BLS_MODULUS
// TODO: Should use a more efficient encoding technique in the future
// Reference: https://github.com/ethpandaops/goomy-blob/blob/e4b460b17b6e2748995ef3d7b75cbe967dc49da4/txbuilder/blob_encode.go#L36
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
        PublishConfig, ADDRESS_ZERO, MAX_USABLE_BLOB_DATA_LEN,
    };

    use super::{construct_blob_tx, encode_data_to_blob};

    const DEV_PRIVKEY: &str = "392a230386a19b84b6b865067d5493b158e987d28104ab16365854a8fd851bb0";

    #[test]
    fn encode_decode_blob_data() {
        let data = vec![0xaa; MAX_USABLE_BLOB_DATA_LEN];
        let blob = encode_data_to_blob(&data);
        assert_eq!(hex::encode(&decode_blob_to_data(&blob)), hex::encode(&data));
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
            &PublishConfig {
                l1_inbox_address: Address::from_str(ADDRESS_ZERO)?,
            },
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
        assert_eq!(
            blob_tx.tx_summary.max_fee_per_gas,
            gas_config.max_fee_per_gas
        );
        assert_eq!(
            blob_tx.tx_summary.max_fee_per_blob_gas,
            gas_config.max_fee_per_blob_gas
        );
        assert_eq!(
            blob_tx.tx_summary.max_priority_fee_per_gas,
            gas_config.max_priority_fee_per_gas
        );

        // Assert participants
        assert_eq!(blob_tx.tx_summary.participants, participants);

        Ok(())
    }
}
