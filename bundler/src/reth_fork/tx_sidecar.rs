use reth_primitives::{keccak256, Signature, TxHash, EIP4844_TX_TYPE_ID};

use alloy_rlp::{Decodable, Encodable, Error as RlpError, Header};
use bytes::BufMut;
use reth_primitives::kzg::{Blob, Bytes48, BYTES_PER_BLOB, BYTES_PER_COMMITMENT, BYTES_PER_PROOF};

use serde::{Deserialize, Serialize};

use super::tx_eip4844::TxEip4844;

/// A response to `GetPooledTransactions` that includes blob data, their commitments, and their
/// corresponding proofs.
///
/// This is defined in [EIP-4844](https://eips.ethereum.org/EIPS/eip-4844#networking) as an element
/// of a `PooledTransactions` response.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BlobTransaction {
    /// The transaction hash.
    pub hash: TxHash,
    /// The transaction payload.
    pub transaction: TxEip4844,
    /// The transaction signature.
    pub signature: Signature,
    /// The transaction's blob sidecar.
    pub sidecar: BlobTransactionSidecar,
}

#[allow(dead_code)]
impl BlobTransaction {
    /// Encodes the [BlobTransaction] fields as RLP, with a tx type. If `with_header` is `false`,
    /// the following will be encoded:
    /// `tx_type (0x03) || rlp([transaction_payload_body, blobs, commitments, proofs])`
    ///
    /// If `with_header` is `true`, the following will be encoded:
    /// `rlp(tx_type (0x03) || rlp([transaction_payload_body, blobs, commitments, proofs]))`
    ///
    /// NOTE: The header will be a byte string header, not a list header.
    pub(crate) fn encode_with_type_inner(&self, out: &mut dyn bytes::BufMut, with_header: bool) {
        // Calculate the length of:
        // `tx_type || rlp([transaction_payload_body, blobs, commitments, proofs])`
        //
        // to construct and encode the string header
        if with_header {
            Header {
                list: false,
                // add one for the tx type
                payload_length: 1 + self.payload_len(),
            }
            .encode(out);
        }

        out.put_u8(EIP4844_TX_TYPE_ID);

        // Now we encode the inner blob transaction:
        self.encode_inner(out);
    }

    /// Encodes the [BlobTransaction] fields as RLP, with the following format:
    /// `rlp([transaction_payload_body, blobs, commitments, proofs])`
    ///
    /// where `transaction_payload_body` is a list:
    /// `[chain_id, nonce, max_priority_fee_per_gas, ..., y_parity, r, s]`
    ///
    /// Note: this should be used only when implementing other RLP encoding methods, and does not
    /// represent the full RLP encoding of the blob transaction.
    pub(crate) fn encode_inner(&self, out: &mut dyn bytes::BufMut) {
        // First we construct both required list headers.
        //
        // The `transaction_payload_body` length is the length of the fields, plus the length of
        // its list header.
        let tx_header = Header {
            list: true,
            payload_length: self.transaction.fields_len() + self.signature.payload_len(),
        };

        let tx_length = tx_header.length() + tx_header.payload_length;

        // The payload length is the length of the `tranascation_payload_body` list, plus the
        // length of the blobs, commitments, and proofs.
        let payload_length = tx_length + self.sidecar.fields_len();

        // First we use the payload len to construct the first list header
        let blob_tx_header = Header {
            list: true,
            payload_length,
        };

        // Encode the blob tx header first
        blob_tx_header.encode(out);

        // Encode the inner tx list header, then its fields
        tx_header.encode(out);
        self.transaction.encode_fields(out);

        // Encode the signature
        self.signature.encode(out);

        // Encode the blobs, commitments, and proofs
        self.sidecar.encode_inner(out);
    }

    /// Ouputs the length of the RLP encoding of the blob transaction, including the tx type byte,
    /// optionally including the length of a wrapping string header. If `with_header` is `false`,
    /// the length of the following will be calculated:
    /// `tx_type (0x03) || rlp([transaction_payload_body, blobs, commitments, proofs])`
    ///
    /// If `with_header` is `true`, the length of the following will be calculated:
    /// `rlp(tx_type (0x03) || rlp([transaction_payload_body, blobs, commitments, proofs]))`
    pub(crate) fn payload_len_with_type(&self, with_header: bool) -> usize {
        if with_header {
            // Construct a header and use that to calculate the total length
            let wrapped_header = Header {
                list: false,
                // add one for the tx type byte
                payload_length: 1 + self.payload_len(),
            };

            // The total length is now the length of the header plus the length of the payload
            // (which includes the tx type byte)
            wrapped_header.length() + wrapped_header.payload_length
        } else {
            // Just add the length of the tx type to the payload length
            1 + self.payload_len()
        }
    }

    /// Outputs the length of the RLP encoding of the blob transaction with the following format:
    /// `rlp([transaction_payload_body, blobs, commitments, proofs])`
    ///
    /// where `transaction_payload_body` is a list:
    /// `[chain_id, nonce, max_priority_fee_per_gas, ..., y_parity, r, s]`
    ///
    /// Note: this should be used only when implementing other RLP encoding length methods, and
    /// does not represent the full RLP encoding of the blob transaction.
    pub(crate) fn payload_len(&self) -> usize {
        // The `transaction_payload_body` length is the length of the fields, plus the length of
        // its list header.
        let tx_header = Header {
            list: true,
            payload_length: self.transaction.fields_len() + self.signature.payload_len(),
        };

        let tx_length = tx_header.length() + tx_header.payload_length;

        // The payload length is the length of the `tranascation_payload_body` list, plus the
        // length of the blobs, commitments, and proofs.
        let payload_length = tx_length + self.sidecar.fields_len();

        // We use the calculated payload len to construct the first list header, which encompasses
        // everything in the tx - the length of the second, inner list header is part of
        // payload_length
        let blob_tx_header = Header {
            list: true,
            payload_length,
        };

        // The final length is the length of:
        //  * the outer blob tx header +
        //  * the inner tx header +
        //  * the inner tx fields +
        //  * the signature fields +
        //  * the sidecar fields
        blob_tx_header.length() + blob_tx_header.payload_length
    }

    /// Decodes a [BlobTransaction] from RLP. This expects the encoding to be:
    /// `rlp([transaction_payload_body, blobs, commitments, proofs])`
    ///
    /// where `transaction_payload_body` is a list:
    /// `[chain_id, nonce, max_priority_fee_per_gas, ..., y_parity, r, s]`
    ///
    /// Note: this should be used only when implementing other RLP decoding methods, and does not
    /// represent the full RLP decoding of the `PooledTransactionsElement` type.
    pub(crate) fn decode_inner(data: &mut &[u8]) -> alloy_rlp::Result<Self> {
        // decode the _first_ list header for the rest of the transaction
        let outer_header = Header::decode(data)?;
        if !outer_header.list {
            return Err(RlpError::Custom(
                "PooledTransactions blob tx must be encoded as a list",
            ));
        }

        let outer_remaining_len = data.len();

        // Now we need to decode the inner 4844 transaction and its signature:
        //
        // `[chain_id, nonce, max_priority_fee_per_gas, ..., y_parity, r, s]`
        let inner_header = Header::decode(data)?;
        if !inner_header.list {
            return Err(RlpError::Custom(
                "PooledTransactions inner blob tx must be encoded as a list",
            ));
        }

        let inner_remaining_len = data.len();

        // inner transaction
        let transaction = TxEip4844::decode_inner(data)?;

        // signature
        let signature = Signature::decode(data)?;

        // the inner header only decodes the transaction and signature, so we check the length here
        let inner_consumed = inner_remaining_len - data.len();
        if inner_consumed != inner_header.payload_length {
            return Err(RlpError::UnexpectedLength);
        }

        // All that's left are the blobs, commitments, and proofs
        let sidecar = BlobTransactionSidecar::decode_inner(data)?;

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
        let mut buf = Vec::new();
        transaction.encode_with_signature(&signature, &mut buf, false);
        let hash = keccak256(&buf);

        // the outer header is for the entire transaction, so we check the length here
        let outer_consumed = outer_remaining_len - data.len();
        if outer_consumed != outer_header.payload_length {
            return Err(RlpError::UnexpectedLength);
        }

        Ok(Self {
            transaction,
            hash,
            signature,
            sidecar,
        })
    }
}

/// This represents a set of blobs, and its corresponding commitments and proofs.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(C)]
pub struct BlobTransactionSidecar {
    /// The blob data.
    pub blobs: Vec<Blob>,
    /// The blob commitments.
    pub commitments: Vec<Bytes48>,
    /// The blob proofs.
    pub proofs: Vec<Bytes48>,
}

impl BlobTransactionSidecar {
    /// Creates a new [BlobTransactionSidecar] using the given blobs, commitments, and proofs.
    pub fn new(blobs: Vec<Blob>, commitments: Vec<Bytes48>, proofs: Vec<Bytes48>) -> Self {
        Self {
            blobs,
            commitments,
            proofs,
        }
    }

    /// Encodes the inner [BlobTransactionSidecar] fields as RLP bytes, without a RLP header.
    ///
    /// This encodes the fields in the following order:
    /// - `blobs`
    /// - `commitments`
    /// - `proofs`
    #[inline]
    pub(crate) fn encode_inner(&self, out: &mut dyn bytes::BufMut) {
        BlobTransactionSidecarRlp::wrap_ref(self).encode(out);
    }

    /// Outputs the RLP length of the [BlobTransactionSidecar] fields, without a RLP header.
    pub fn fields_len(&self) -> usize {
        BlobTransactionSidecarRlp::wrap_ref(self).fields_len()
    }

    /// Decodes the inner [BlobTransactionSidecar] fields from RLP bytes, without a RLP header.
    ///
    /// This decodes the fields in the following order:
    /// - `blobs`
    /// - `commitments`
    /// - `proofs`
    #[inline]
    pub(crate) fn decode_inner(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Ok(BlobTransactionSidecarRlp::decode(buf)?.unwrap())
    }

    /// Calculates a size heuristic for the in-memory size of the [BlobTransactionSidecar].
    #[inline]
    pub fn size(&self) -> usize {
        self.blobs.len() * BYTES_PER_BLOB + // blobs
        self.commitments.len() * BYTES_PER_COMMITMENT + // commitments
        self.proofs.len() * BYTES_PER_PROOF // proofs
    }
}

impl From<reth_rpc_types::BlobTransactionSidecar> for BlobTransactionSidecar {
    fn from(value: reth_rpc_types::BlobTransactionSidecar) -> Self {
        // SAFETY: Same repr and size
        unsafe { std::mem::transmute(value) }
    }
}

impl From<BlobTransactionSidecar> for reth_rpc_types::BlobTransactionSidecar {
    fn from(value: BlobTransactionSidecar) -> Self {
        // SAFETY: Same repr and size
        unsafe { std::mem::transmute(value) }
    }
}

impl Encodable for BlobTransactionSidecar {
    /// Encodes the inner [BlobTransactionSidecar] fields as RLP bytes, without a RLP header.
    fn encode(&self, out: &mut dyn BufMut) {
        self.encode_inner(out)
    }

    fn length(&self) -> usize {
        self.fields_len()
    }
}

impl Decodable for BlobTransactionSidecar {
    /// Decodes the inner [BlobTransactionSidecar] fields from RLP bytes, without a RLP header.
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Self::decode_inner(buf)
    }
}

// Wrapper for c-kzg rlp
#[repr(C)]
struct BlobTransactionSidecarRlp {
    blobs: Vec<[u8; c_kzg::BYTES_PER_BLOB]>,
    commitments: Vec<[u8; 48]>,
    proofs: Vec<[u8; 48]>,
}

const _: [(); std::mem::size_of::<BlobTransactionSidecar>()] =
    [(); std::mem::size_of::<BlobTransactionSidecarRlp>()];

const _: [(); std::mem::size_of::<BlobTransactionSidecar>()] =
    [(); std::mem::size_of::<reth_rpc_types::BlobTransactionSidecar>()];

impl BlobTransactionSidecarRlp {
    fn wrap_ref(other: &BlobTransactionSidecar) -> &Self {
        // SAFETY: Same repr and size
        unsafe { &*(other as *const BlobTransactionSidecar).cast::<Self>() }
    }

    fn unwrap(self) -> BlobTransactionSidecar {
        // SAFETY: Same repr and size
        unsafe { std::mem::transmute(self) }
    }

    fn encode(&self, out: &mut dyn bytes::BufMut) {
        // Encode the blobs, commitments, and proofs
        self.blobs.encode(out);
        self.commitments.encode(out);
        self.proofs.encode(out);
    }

    fn fields_len(&self) -> usize {
        self.blobs.length() + self.commitments.length() + self.proofs.length()
    }

    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Ok(Self {
            blobs: Decodable::decode(buf)?,
            commitments: Decodable::decode(buf)?,
            proofs: Decodable::decode(buf)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use alloy_rlp::Encodable;
    use reth_primitives::{AccessList, Bytes as RethBytes, TxValue};

    use super::super::tx_eip4844::TxEip4844;

    fn minimal_tx() -> TxEip4844 {
        TxEip4844 {
            chain_id: 1,
            nonce: 0,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 30_000_000_000,
            gas_limit: 21_000,
            to: reth_primitives::Address::ZERO,
            value: TxValue::from(0u64),
            access_list: AccessList::default(),
            blob_versioned_hashes: vec![reth_primitives::B256::ZERO],
            max_fee_per_blob_gas: 1_000_000_000,
            input: RethBytes::default(),
        }
    }

    fn test_signature() -> Signature {
        Signature {
            r: U256::from(1u64),
            s: U256::from(2u64),
            odd_y_parity: false,
        }
    }

    fn dummy_blob() -> Blob {
        Blob::from_bytes(&[0u8; c_kzg::BYTES_PER_BLOB]).unwrap()
    }

    fn dummy_commitment() -> Bytes48 {
        Bytes48::from_bytes(&[0xab; 48]).unwrap()
    }

    fn dummy_proof() -> Bytes48 {
        Bytes48::from_bytes(&[0xcd; 48]).unwrap()
    }

    fn minimal_sidecar() -> BlobTransactionSidecar {
        BlobTransactionSidecar::new(
            vec![dummy_blob()],
            vec![dummy_commitment()],
            vec![dummy_proof()],
        )
    }

    fn minimal_blob_tx() -> BlobTransaction {
        let tx = minimal_tx();
        let sig = test_signature();
        let mut buf = Vec::new();
        tx.encode_with_signature(&sig, &mut buf, false);
        let hash = keccak256(&buf);
        BlobTransaction {
            hash,
            transaction: tx,
            signature: sig,
            sidecar: minimal_sidecar(),
        }
    }

    // -- BlobTransactionSidecar --

    #[test]
    fn sidecar_new_stores_fields() {
        let blobs = vec![dummy_blob()];
        let commitments = vec![dummy_commitment()];
        let proofs = vec![dummy_proof()];
        let sidecar =
            BlobTransactionSidecar::new(blobs.clone(), commitments.clone(), proofs.clone());
        assert_eq!(sidecar.blobs.len(), 1);
        assert_eq!(sidecar.commitments.len(), 1);
        assert_eq!(sidecar.proofs.len(), 1);
    }

    #[test]
    fn sidecar_size_single_blob() {
        let sidecar = minimal_sidecar();
        assert_eq!(
            sidecar.size(),
            BYTES_PER_BLOB + BYTES_PER_COMMITMENT + BYTES_PER_PROOF
        );
    }

    #[test]
    fn sidecar_size_empty() {
        let sidecar = BlobTransactionSidecar::new(vec![], vec![], vec![]);
        assert_eq!(sidecar.size(), 0);
    }

    #[test]
    fn sidecar_encode_decode_roundtrip() {
        let sidecar = minimal_sidecar();

        let mut buf = Vec::new();
        sidecar.encode_inner(&mut buf);

        let mut slice = buf.as_slice();
        let decoded = BlobTransactionSidecar::decode_inner(&mut slice).unwrap();
        assert!(slice.is_empty(), "all bytes should be consumed");
        assert_eq!(decoded, sidecar);
    }

    #[test]
    fn sidecar_fields_len_matches_encoded_len() {
        let sidecar = minimal_sidecar();
        let mut buf = Vec::new();
        sidecar.encode_inner(&mut buf);
        assert_eq!(sidecar.fields_len(), buf.len());
    }

    #[test]
    fn sidecar_encodable_trait_matches_encode_inner() {
        let sidecar = minimal_sidecar();

        let mut buf_inner = Vec::new();
        sidecar.encode_inner(&mut buf_inner);

        let mut buf_trait = Vec::new();
        Encodable::encode(&sidecar, &mut buf_trait);

        assert_eq!(buf_inner, buf_trait);
    }

    #[test]
    fn sidecar_decodable_trait_matches_decode_inner() {
        let sidecar = minimal_sidecar();
        let mut buf = Vec::new();
        sidecar.encode_inner(&mut buf);

        let mut slice1 = buf.as_slice();
        let decoded1 = BlobTransactionSidecar::decode_inner(&mut slice1).unwrap();

        let mut slice2 = buf.as_slice();
        let decoded2 = <BlobTransactionSidecar as Decodable>::decode(&mut slice2).unwrap();

        assert_eq!(decoded1, decoded2);
    }

    #[test]
    fn sidecar_empty_encode_decode_roundtrip() {
        let sidecar = BlobTransactionSidecar::new(vec![], vec![], vec![]);
        let mut buf = Vec::new();
        sidecar.encode_inner(&mut buf);

        let mut slice = buf.as_slice();
        let decoded = BlobTransactionSidecar::decode_inner(&mut slice).unwrap();
        assert_eq!(decoded, sidecar);
    }

    // -- BlobTransaction --

    #[test]
    fn blob_tx_encode_decode_roundtrip() {
        let blob_tx = minimal_blob_tx();

        // Encode: tx_type || rlp(...)
        let mut buf = Vec::new();
        blob_tx.encode_with_type_inner(&mut buf, false);

        // First byte is EIP-4844 tx type
        assert_eq!(buf[0], EIP4844_TX_TYPE_ID);

        // Decode (skip the tx type byte)
        let mut slice = &buf[1..];
        let decoded = BlobTransaction::decode_inner(&mut slice).unwrap();
        assert!(slice.is_empty(), "all bytes should be consumed");

        assert_eq!(decoded.transaction, blob_tx.transaction);
        assert_eq!(decoded.signature, blob_tx.signature);
        assert_eq!(decoded.sidecar, blob_tx.sidecar);
        assert_eq!(decoded.hash, blob_tx.hash);
    }

    #[test]
    fn blob_tx_payload_len_with_type_no_header_matches_encoded() {
        let blob_tx = minimal_blob_tx();

        let mut buf = Vec::new();
        blob_tx.encode_with_type_inner(&mut buf, false);

        assert_eq!(blob_tx.payload_len_with_type(false), buf.len());
    }

    #[test]
    fn blob_tx_payload_len_with_type_with_header_matches_encoded() {
        let blob_tx = minimal_blob_tx();

        let mut buf = Vec::new();
        blob_tx.encode_with_type_inner(&mut buf, true);

        assert_eq!(blob_tx.payload_len_with_type(true), buf.len());
    }

    #[test]
    fn blob_tx_with_header_wraps_without_header() {
        let blob_tx = minimal_blob_tx();

        let mut with_header = Vec::new();
        blob_tx.encode_with_type_inner(&mut with_header, true);

        let mut without_header = Vec::new();
        blob_tx.encode_with_type_inner(&mut without_header, false);

        // With header should be strictly longer
        assert!(with_header.len() > without_header.len());

        // Decode the outer RLP header to get the inner content
        let mut slice = with_header.as_slice();
        let header = alloy_rlp::Header::decode(&mut slice).unwrap();
        assert!(!header.list); // byte string header
        assert_eq!(slice, without_header.as_slice());
    }

    #[test]
    fn blob_tx_hash_is_keccak_of_signed_encoding() {
        let blob_tx = minimal_blob_tx();

        // The hash should be keccak256(tx_type || rlp(tx_payload_body))
        // which is what encode_with_signature produces (without header)
        let mut buf = Vec::new();
        blob_tx
            .transaction
            .encode_with_signature(&blob_tx.signature, &mut buf, false);
        let expected_hash = keccak256(&buf);

        assert_eq!(blob_tx.hash, expected_hash);
    }

    #[test]
    fn blob_tx_decode_inner_rejects_non_list_outer() {
        // Encode a byte string header instead of list header
        let mut buf = Vec::new();
        alloy_rlp::Header {
            list: false,
            payload_length: 10,
        }
        .encode(&mut buf);
        buf.extend_from_slice(&[0; 10]);

        let mut slice = buf.as_slice();
        assert!(BlobTransaction::decode_inner(&mut slice).is_err());
    }

    #[test]
    fn blob_tx_payload_len_consistency() {
        let blob_tx = minimal_blob_tx();

        // payload_len_with_type(true) should equal
        // header_len + 1 (tx_type) + payload_len()
        let inner_payload = blob_tx.payload_len();
        let with_type_no_header = 1 + inner_payload;
        assert_eq!(blob_tx.payload_len_with_type(false), with_type_no_header);
    }

    // -- reth_rpc_types conversion --

    #[test]
    fn sidecar_from_reth_rpc_types_roundtrip() {
        let sidecar = minimal_sidecar();

        // Convert to reth_rpc_types and back
        let reth_sidecar: reth_rpc_types::BlobTransactionSidecar = sidecar.clone().into();
        let roundtripped: BlobTransactionSidecar = reth_sidecar.into();

        assert_eq!(roundtripped, sidecar);
    }
}
