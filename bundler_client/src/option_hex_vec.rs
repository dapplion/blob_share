//! Formats `Option<Vec<u8>>` as a 0x-prefixed hex string.
//!
//! E.g., `vec![0, 1, 2, 3]` serializes as `"0x00010203"`.

use serde::{
    de::{self, Visitor},
    Deserializer, Serializer,
};
use serde_utils::hex::PrefixedHexVisitor;
use std::fmt;

pub fn serialize<S>(bytes: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match bytes {
        Some(bytes) => {
            let mut hex_string: String = "0x".to_string();
            hex_string.push_str(&hex::encode(bytes));

            serializer.serialize_str(&hex_string)
        }
        None => serializer.serialize_none(),
    }
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_option(OptionPrefixedHexVisitor)
}

pub struct OptionPrefixedHexVisitor;

impl<'de> Visitor<'de> for OptionPrefixedHexVisitor {
    type Value = Option<Vec<u8>>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a hex string with 0x prefix or null")
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Some(deserializer.deserialize_string(PrefixedHexVisitor)?))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    /// Wrapper struct to exercise the custom serde module via `#[serde(with = ...)]`.
    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Wrapper {
        #[serde(with = "super")]
        value: Option<Vec<u8>>,
    }

    #[test]
    fn serialize_some_bytes() {
        let w = Wrapper {
            value: Some(vec![0x00, 0x01, 0x02, 0xff]),
        };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"value":"0x000102ff"}"#);
    }

    #[test]
    fn serialize_none() {
        let w = Wrapper { value: None };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"value":null}"#);
    }

    #[test]
    fn serialize_some_empty() {
        let w = Wrapper {
            value: Some(vec![]),
        };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"value":"0x"}"#);
    }

    #[test]
    fn deserialize_some_with_prefix() {
        let json = r#"{"value":"0xdeadbeef"}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(w.value, Some(vec![0xde, 0xad, 0xbe, 0xef]));
    }

    #[test]
    fn deserialize_null() {
        let json = r#"{"value":null}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(w.value, None);
    }

    #[test]
    fn deserialize_empty_hex() {
        let json = r#"{"value":"0x"}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(w.value, Some(vec![]));
    }

    #[test]
    fn roundtrip_some() {
        let w = Wrapper {
            value: Some(vec![0xaa, 0xbb, 0xcc]),
        };
        let json = serde_json::to_string(&w).unwrap();
        let w2: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(w, w2);
    }

    #[test]
    fn roundtrip_none() {
        let w = Wrapper { value: None };
        let json = serde_json::to_string(&w).unwrap();
        let w2: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(w, w2);
    }

    #[test]
    fn roundtrip_single_byte() {
        let w = Wrapper {
            value: Some(vec![0x42]),
        };
        let json = serde_json::to_string(&w).unwrap();
        let w2: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(w, w2);
    }

    #[test]
    fn deserialize_invalid_hex_fails() {
        let json = r#"{"value":"0xZZZZ"}"#;
        let result = serde_json::from_str::<Wrapper>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_odd_length_hex_fails() {
        // Odd number of hex chars after 0x prefix
        let json = r#"{"value":"0xabc"}"#;
        let result = serde_json::from_str::<Wrapper>(json);
        assert!(result.is_err());
    }

    #[test]
    fn serialize_preserves_zero_bytes() {
        let w = Wrapper {
            value: Some(vec![0x00, 0x00, 0x00]),
        };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"value":"0x000000"}"#);
        let w2: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(w, w2);
    }
}
