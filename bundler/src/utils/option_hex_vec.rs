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

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Wrapper {
        #[serde(with = "super")]
        data: Option<Vec<u8>>,
    }

    #[test]
    fn serialize_some_bytes() {
        let w = Wrapper {
            data: Some(vec![0x00, 0x01, 0x02, 0x03]),
        };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"data":"0x00010203"}"#);
    }

    #[test]
    fn serialize_none() {
        let w = Wrapper { data: None };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"data":null}"#);
    }

    #[test]
    fn serialize_empty_bytes() {
        let w = Wrapper { data: Some(vec![]) };
        let json = serde_json::to_string(&w).unwrap();
        assert_eq!(json, r#"{"data":"0x"}"#);
    }

    #[test]
    fn deserialize_some_bytes() {
        let json = r#"{"data":"0x00010203"}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(
            w,
            Wrapper {
                data: Some(vec![0x00, 0x01, 0x02, 0x03]),
            }
        );
    }

    #[test]
    fn deserialize_null() {
        let json = r#"{"data":null}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(w, Wrapper { data: None });
    }

    #[test]
    fn deserialize_empty_hex() {
        let json = r#"{"data":"0x"}"#;
        let w: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(w, Wrapper { data: Some(vec![]) });
    }

    #[test]
    fn roundtrip_some() {
        let original = Wrapper {
            data: Some(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        };
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn roundtrip_none() {
        let original = Wrapper { data: None };
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn deserialize_invalid_hex_returns_error() {
        let json = r#"{"data":"0xZZZZ"}"#;
        let result = serde_json::from_str::<Wrapper>(json);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_no_prefix_returns_error() {
        let json = r#"{"data":"00010203"}"#;
        let result = serde_json::from_str::<Wrapper>(json);
        assert!(result.is_err());
    }
}
