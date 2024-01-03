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
