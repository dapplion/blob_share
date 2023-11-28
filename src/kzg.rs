use std::cmp::Ordering;

use alloy_primitives::{keccak256, Address, B256, U256};
use c_kzg::{BYTES_PER_BLOB, FIELD_ELEMENTS_PER_BLOB};
use ethers::{
    providers::{Http, Middleware, Provider},
    signers::{to_eip155_v, LocalWallet},
    types::{Bytes, H256},
};
use eyre::{bail, Result};
use reth_primitives::Signature;
use sha2::{Digest, Sha256};

use crate::{
    gas::GasConfig,
    tx_eip4844::TxEip4844,
    tx_sidecar::{BlobTransaction, BlobTransactionSidecar},
    DataIntent, PublishConfig,
};

pub const VERSIONED_HASH_VERSION_KZG: u8 = 0x01;

pub struct BlobTx {
    pub blob_tx_payload_body: Bytes,
    pub tx_hash: B256,
    pub blob_tx_networking: Bytes,
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
    wallet: &LocalWallet,
    next_blob_items: Vec<DataIntent>,
) -> Result<BlobTx> {
    let mut data = vec![];
    for mut item in next_blob_items.into_iter() {
        data.append(&mut item.data);
    }

    // TODO: should chunk data in 31 bytes to ensure each field element if < BLS_MODULUS
    // pad data to fit blob
    let target_data_len = FIELD_ELEMENTS_PER_BLOB * 31;
    match data.len().cmp(&target_data_len) {
        Ordering::Less => data.extend(vec![0; target_data_len - data.len()]),
        Ordering::Equal => {}
        Ordering::Greater => bail!("data longer than blob capacity"),
    }

    // Chunk data in 31 bytes to ensure each field element is < BLS_MODULUS
    // TODO: Should use a more efficient encoding technique in the future
    // Reference: https://github.com/ethpandaops/goomy-blob/blob/e4b460b17b6e2748995ef3d7b75cbe967dc49da4/txbuilder/blob_encode.go#L36
    let mut chunked_blob_data = vec![0u8; BYTES_PER_BLOB];
    for (field_index, chunk) in data.chunks(31).enumerate() {
        chunked_blob_data[field_index * 32 + 1..field_index * 32 + 32].copy_from_slice(chunk);
    }

    let blob = c_kzg::Blob::from_bytes(&chunked_blob_data)?;
    let commitment = c_kzg::KzgCommitment::blob_to_kzg_commitment(&blob, &kzg_settings)?;
    let versioned_hash = kzg_to_versioned_hash(&commitment);
    let proof =
        c_kzg::KzgProof::compute_blob_kzg_proof(&blob, &commitment.to_bytes(), &kzg_settings)?
            .to_bytes();

    let tx = TxEip4844 {
        chain_id: <_>::default(),
        nonce: <_>::default(),
        max_priority_fee_per_gas: gas_config.max_priority_fee_per_gas,
        max_fee_per_gas: gas_config.max_fee_per_gas,
        gas_limit: <_>::default(),
        to: Address::from(publish_config.l1_inbox_address.to_fixed_bytes()),
        value: <_>::default(),
        input: <_>::default(),
        access_list: <_>::default(),
        max_fee_per_blob_gas: gas_config.max_fee_per_blob_gas,
        blob_versioned_hashes: vec![versioned_hash],
    };

    let sigature_hash = tx.signature_hash();

    let mut signature = wallet.sign_hash(H256::from_slice(&sigature_hash.to_vec()))?;
    // sign_hash sets `v` to recid + 27, so we need to subtract 27 before normalizing
    signature.v = to_eip155_v(signature.v as u8 - 27, tx.chain_id);

    // Convert Signature from ethers-rs to alloy
    let signature = Signature {
        r: U256::from_be_bytes(signature.r.into()),
        s: U256::from_be_bytes(signature.s.into()),
        odd_y_parity: signature.v != 0,
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
    blob_tx.encode_with_type_inner(&mut tx_rlp_networking, true);

    Ok(BlobTx {
        blob_tx_payload_body: tx_rlp_with_sig.into(),
        tx_hash,
        blob_tx_networking: tx_rlp_networking.into(),
    })
}

pub(crate) async fn send_blob_tx(provider: &Provider<Http>, tx: BlobTx) -> Result<()> {
    // broadcast it via the eth_sendTransaction API
    let tx = provider.send_raw_transaction(tx.blob_tx_networking).await?;

    log::info!(
        "Sent blob transaction {} {:?}",
        &tx.tx_hash().to_string(),
        &tx
    );

    let tx = tx.confirmations(0);
    let receipt = tx.await?;

    log::info!("Confirmed blob transaction {:?}", &receipt);

    Ok(())
}
