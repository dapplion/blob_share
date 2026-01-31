use alloy_rlp::{length_of_length, Decodable, Encodable, Header};
use bytes::BytesMut;
use reth_primitives::{
    keccak256, AccessList, Address, Bytes, ChainId, Signature, TxType, TxValue, B256,
};
use serde::{Deserialize, Serialize};
use std::mem;

/// [EIP-4844 Blob Transaction](https://eips.ethereum.org/EIPS/eip-4844#blob-transaction)
///
/// A transaction with blob hashes and max blob fee
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct TxEip4844 {
    /// Added as EIP-pub 155: Simple replay attack protection
    pub chain_id: u64,
    /// A scalar value equal to the number of transactions sent by the sender; formally Tn.
    pub nonce: u64,
    /// A scalar value equal to the maximum
    /// amount of gas that should be used in executing
    /// this transaction. This is paid up-front, before any
    /// computation is done and may not be increased
    /// later; formally Tg.
    pub gas_limit: u64,
    /// A scalar value equal to the maximum
    /// amount of gas that should be used in executing
    /// this transaction. This is paid up-front, before any
    /// computation is done and may not be increased
    /// later; formally Tg.
    ///
    /// As ethereum circulation is around 120mil eth as of 2022 that is around
    /// 120000000000000000000000000 wei we are safe to use u128 as its max number is:
    /// 340282366920938463463374607431768211455
    ///
    /// This is also known as `GasFeeCap`
    pub max_fee_per_gas: u128,
    /// Max Priority fee that transaction is paying
    ///
    /// As ethereum circulation is around 120mil eth as of 2022 that is around
    /// 120000000000000000000000000 wei we are safe to use u128 as its max number is:
    /// 340282366920938463463374607431768211455
    ///
    /// This is also known as `GasTipCap`
    pub max_priority_fee_per_gas: u128,
    /// The 160-bit address of the message call’s recipient or, for a contract creation
    /// transaction, ∅, used here to denote the only member of B0 ; formally Tt.
    pub to: Address,
    /// A scalar value equal to the number of Wei to
    /// be transferred to the message call’s recipient or,
    /// in the case of contract creation, as an endowment
    /// to the newly created account; formally Tv.
    pub value: TxValue,
    /// The accessList specifies a list of addresses and storage keys;
    /// these addresses and storage keys are added into the `accessed_addresses`
    /// and `accessed_storage_keys` global sets (introduced in EIP-2929).
    /// A gas cost is charged, though at a discount relative to the cost of
    /// accessing outside the list.
    pub access_list: AccessList,

    /// It contains a vector of fixed size hash(32 bytes)
    pub blob_versioned_hashes: Vec<B256>,

    /// Max fee per data gas
    ///
    /// aka BlobFeeCap or blobGasFeeCap
    pub max_fee_per_blob_gas: u128,

    /// Input has two uses depending if transaction is Create or Call (if `to` field is None or
    /// Some). pub init: An unlimited size byte array specifying the
    /// EVM-code for the account initialisation procedure CREATE,
    /// data: An unlimited size byte array specifying the
    /// input data of the message call, formally Td.
    pub input: Bytes,
}

#[allow(dead_code)]
impl TxEip4844 {
    /// Decodes the inner [TxEip4844] fields from RLP bytes.
    ///
    /// NOTE: This assumes a RLP header has already been decoded, and _just_ decodes the following
    /// RLP fields in the following order:
    ///
    /// - `chain_id`
    /// - `nonce`
    /// - `max_priority_fee_per_gas`
    /// - `max_fee_per_gas`
    /// - `gas_limit`
    /// - `to`
    /// - `value`
    /// - `data` (`input`)
    /// - `access_list`
    /// - `max_fee_per_blob_gas`
    /// - `blob_versioned_hashes`
    pub fn decode_inner(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        Ok(Self {
            chain_id: Decodable::decode(buf)?,
            nonce: Decodable::decode(buf)?,
            max_priority_fee_per_gas: Decodable::decode(buf)?,
            max_fee_per_gas: Decodable::decode(buf)?,
            gas_limit: Decodable::decode(buf)?,
            to: Decodable::decode(buf)?,
            value: Decodable::decode(buf)?,
            input: Decodable::decode(buf)?,
            access_list: Decodable::decode(buf)?,
            max_fee_per_blob_gas: Decodable::decode(buf)?,
            blob_versioned_hashes: Decodable::decode(buf)?,
        })
    }

    /// Outputs the length of the transaction's fields, without a RLP header.
    pub(crate) fn fields_len(&self) -> usize {
        let mut len = 0;
        len += self.chain_id.length();
        len += self.nonce.length();
        len += self.gas_limit.length();
        len += self.max_fee_per_gas.length();
        len += self.max_priority_fee_per_gas.length();
        len += self.to.length();
        len += self.value.length();
        len += self.access_list.length();
        len += self.blob_versioned_hashes.length();
        len += self.max_fee_per_blob_gas.length();
        len += self.input.0.length();
        len
    }

    /// Encodes only the transaction's fields into the desired buffer, without a RLP header.
    pub(crate) fn encode_fields(&self, out: &mut dyn bytes::BufMut) {
        self.chain_id.encode(out);
        self.nonce.encode(out);
        self.max_priority_fee_per_gas.encode(out);
        self.max_fee_per_gas.encode(out);
        self.gas_limit.encode(out);
        self.to.encode(out);
        self.value.encode(out);
        self.input.0.encode(out);
        self.access_list.encode(out);
        self.max_fee_per_blob_gas.encode(out);
        self.blob_versioned_hashes.encode(out);
    }

    /// Calculates a heuristic for the in-memory size of the [TxEip4844] transaction.
    #[inline]
    pub(crate) fn size(&self) -> usize {
        mem::size_of::<ChainId>() + // chain_id
        mem::size_of::<u64>() + // nonce
        mem::size_of::<u64>() + // gas_limit
        mem::size_of::<u128>() + // max_fee_per_gas
        mem::size_of::<u128>() + // max_priority_fee_per_gas
        mem::size_of::<Address>() + // 
        mem::size_of::<TxValue>() + // value
        self.access_list.size() + // access_list
        self.input.len() +  // input
        self.blob_versioned_hashes.capacity() * mem::size_of::<B256>() + // blob hashes size
        mem::size_of::<u128>() // max_fee_per_data_gas
    }

    /// Inner encoding function that is used for both rlp [`Encodable`] trait and for calculating
    /// hash that for eip2718 does not require rlp header
    pub(crate) fn encode_with_signature(
        &self,
        signature: &Signature,
        out: &mut dyn bytes::BufMut,
        with_header: bool,
    ) {
        let payload_length = self.fields_len() + signature.payload_len();
        if with_header {
            Header {
                list: false,
                payload_length: 1 + length_of_length(payload_length) + payload_length,
            }
            .encode(out);
        }
        out.put_u8(self.tx_type() as u8);
        let header = Header {
            list: true,
            payload_length,
        };
        header.encode(out);
        self.encode_fields(out);
        signature.encode(out);
    }

    /// Output the length of the RLP signed transaction encoding. This encodes with a RLP header.
    pub(crate) fn payload_len_with_signature(&self, signature: &Signature) -> usize {
        let len = self.payload_len_with_signature_without_header(signature);
        length_of_length(len) + len
    }

    /// Output the length of the RLP signed transaction encoding, _without_ a RLP header.
    pub(crate) fn payload_len_with_signature_without_header(&self, signature: &Signature) -> usize {
        let payload_length = self.fields_len() + signature.payload_len();
        // 'transaction type byte length' + 'header length' + 'payload length'
        1 + length_of_length(payload_length) + payload_length
    }

    /// Get transaction type
    pub(crate) fn tx_type(&self) -> TxType {
        TxType::EIP4844
    }

    /// Encodes the legacy transaction in RLP for signing.
    pub(crate) fn encode_for_signing(&self, out: &mut dyn bytes::BufMut) {
        out.put_u8(self.tx_type() as u8);
        Header {
            list: true,
            payload_length: self.fields_len(),
        }
        .encode(out);
        self.encode_fields(out);
    }

    /// Outputs the length of the signature RLP encoding for the transaction.
    pub(crate) fn payload_len_for_signature(&self) -> usize {
        let payload_length = self.fields_len();
        // 'transaction type byte length' + 'header length' + 'payload length'
        1 + length_of_length(payload_length) + payload_length
    }

    /// Outputs the signature hash of the transaction by first encoding without a signature, then
    /// hashing.
    pub(crate) fn signature_hash(&self) -> B256 {
        let mut buf = BytesMut::with_capacity(self.payload_len_for_signature());
        self.encode_for_signing(&mut buf);
        keccak256(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;

    /// Build a minimal TxEip4844 for testing.
    fn minimal_tx() -> TxEip4844 {
        TxEip4844 {
            chain_id: 1,
            nonce: 0,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 30_000_000_000,
            gas_limit: 21_000,
            to: Address::ZERO,
            value: TxValue::from(0u64),
            access_list: AccessList::default(),
            blob_versioned_hashes: vec![B256::ZERO],
            max_fee_per_blob_gas: 1_000_000_000,
            input: Bytes::default(),
        }
    }

    fn test_signature() -> Signature {
        Signature {
            r: U256::from(1u64),
            s: U256::from(2u64),
            odd_y_parity: false,
        }
    }

    // -- fields_len --

    #[test]
    fn fields_len_default_tx_is_nonzero() {
        let tx = TxEip4844::default();
        // Even a default tx has non-zero field lengths from chain_id, addresses, etc.
        assert!(tx.fields_len() > 0);
    }

    #[test]
    fn fields_len_increases_with_input() {
        let tx1 = minimal_tx();
        let mut tx2 = minimal_tx();
        tx2.input = Bytes::from(vec![0xaa; 100]);
        assert!(tx2.fields_len() > tx1.fields_len());
    }

    #[test]
    fn fields_len_increases_with_blob_hashes() {
        let tx1 = minimal_tx();
        let mut tx2 = minimal_tx();
        tx2.blob_versioned_hashes = vec![B256::ZERO, B256::ZERO, B256::ZERO];
        assert!(tx2.fields_len() > tx1.fields_len());
    }

    // -- encode_fields / decode_inner roundtrip --

    #[test]
    fn encode_decode_roundtrip() {
        let tx = minimal_tx();

        // Encode fields wrapped in a list header
        let mut buf = BytesMut::new();
        let header = Header {
            list: true,
            payload_length: tx.fields_len(),
        };
        header.encode(&mut buf);
        tx.encode_fields(&mut buf);

        // Decode: first consume the list header, then decode fields
        let mut slice = buf.as_ref();
        let decoded_header = Header::decode(&mut slice).unwrap();
        assert!(decoded_header.list);
        assert_eq!(decoded_header.payload_length, tx.fields_len());

        let decoded = TxEip4844::decode_inner(&mut slice).unwrap();
        assert_eq!(decoded, tx);
        assert!(slice.is_empty(), "all bytes should be consumed");
    }

    #[test]
    fn encode_decode_roundtrip_with_input_and_access_list() {
        let mut tx = minimal_tx();
        tx.input = Bytes::from(vec![0x01, 0x02, 0x03]);
        tx.nonce = 42;
        tx.chain_id = 5;
        tx.max_fee_per_gas = 100_000_000_000;
        tx.blob_versioned_hashes = vec![B256::from([0xab; 32]), B256::from([0xcd; 32])];

        let mut buf = BytesMut::new();
        let header = Header {
            list: true,
            payload_length: tx.fields_len(),
        };
        header.encode(&mut buf);
        tx.encode_fields(&mut buf);

        let mut slice = buf.as_ref();
        let _ = Header::decode(&mut slice).unwrap();
        let decoded = TxEip4844::decode_inner(&mut slice).unwrap();
        assert_eq!(decoded, tx);
    }

    // -- encode_with_signature --

    #[test]
    fn encode_with_signature_starts_with_tx_type() {
        let tx = minimal_tx();
        let sig = test_signature();

        // Without header: raw bytes start with tx type byte (0x03)
        let mut buf = Vec::new();
        tx.encode_with_signature(&sig, &mut buf, false);
        assert_eq!(buf[0], TxType::EIP4844 as u8);
    }

    #[test]
    fn encode_with_signature_with_header_has_rlp_prefix() {
        let tx = minimal_tx();
        let sig = test_signature();

        let mut with_header = Vec::new();
        tx.encode_with_signature(&sig, &mut with_header, true);

        let mut without_header = Vec::new();
        tx.encode_with_signature(&sig, &mut without_header, false);

        // With header should be longer (has the RLP string header wrapping it)
        assert!(with_header.len() > without_header.len());
        // The content after decoding the header should match
        let mut slice = with_header.as_slice();
        let header = Header::decode(&mut slice).unwrap();
        assert!(!header.list); // byte string header, not list
        assert_eq!(slice, &without_header);
    }

    // -- payload_len_with_signature --

    #[test]
    fn payload_len_with_signature_matches_encoded_len() {
        let tx = minimal_tx();
        let sig = test_signature();

        let mut buf = Vec::new();
        tx.encode_with_signature(&sig, &mut buf, true);

        assert_eq!(tx.payload_len_with_signature(&sig), buf.len());
    }

    #[test]
    fn payload_len_without_header_matches_encoded_len() {
        let tx = minimal_tx();
        let sig = test_signature();

        let mut buf = Vec::new();
        tx.encode_with_signature(&sig, &mut buf, false);

        assert_eq!(
            tx.payload_len_with_signature_without_header(&sig),
            buf.len()
        );
    }

    // -- encode_for_signing / payload_len_for_signature --

    #[test]
    fn encode_for_signing_starts_with_tx_type() {
        let tx = minimal_tx();
        let mut buf = BytesMut::new();
        tx.encode_for_signing(&mut buf);
        assert_eq!(buf[0], TxType::EIP4844 as u8);
    }

    #[test]
    fn payload_len_for_signature_matches_encoded_len() {
        let tx = minimal_tx();
        let mut buf = BytesMut::new();
        tx.encode_for_signing(&mut buf);
        assert_eq!(tx.payload_len_for_signature(), buf.len());
    }

    // -- signature_hash --

    #[test]
    fn signature_hash_is_deterministic() {
        let tx = minimal_tx();
        assert_eq!(tx.signature_hash(), tx.signature_hash());
    }

    #[test]
    fn signature_hash_changes_with_nonce() {
        let tx1 = minimal_tx();
        let mut tx2 = minimal_tx();
        tx2.nonce = 999;
        assert_ne!(tx1.signature_hash(), tx2.signature_hash());
    }

    #[test]
    fn signature_hash_changes_with_chain_id() {
        let tx1 = minimal_tx();
        let mut tx2 = minimal_tx();
        tx2.chain_id = 5;
        assert_ne!(tx1.signature_hash(), tx2.signature_hash());
    }

    // -- tx_type --

    #[test]
    fn tx_type_is_eip4844() {
        let tx = minimal_tx();
        assert_eq!(tx.tx_type(), TxType::EIP4844);
    }

    // -- size --

    #[test]
    fn size_increases_with_input() {
        let tx1 = minimal_tx();
        let mut tx2 = minimal_tx();
        tx2.input = Bytes::from(vec![0xff; 256]);
        assert!(tx2.size() > tx1.size());
    }

    // -- decode_inner error cases --

    #[test]
    fn decode_inner_fails_on_truncated_input() {
        // Encode a valid tx, then truncate the fields-only portion
        let tx = minimal_tx();
        let mut buf = BytesMut::new();
        tx.encode_fields(&mut buf);

        // Truncate to half: decode_inner should fail with incomplete data
        let truncated = &buf[..buf.len() / 2];
        let mut slice = truncated;
        assert!(TxEip4844::decode_inner(&mut slice).is_err());
    }

    #[test]
    fn decode_inner_fails_on_empty_input() {
        let mut empty: &[u8] = &[];
        assert!(TxEip4844::decode_inner(&mut empty).is_err());
    }
}
