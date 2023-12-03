use serde::{
    de::{self, Visitor},
    Deserializer, Serializer,
};
use std::fmt::{self, Debug, Display};

// Return an opaque 500 while preserving the error root's cause for logging.
pub(crate) fn e500<T>(e: T) -> actix_web::Error
where
    T: Debug + Display + 'static,
{
    actix_web::error::ErrorInternalServerError(e)
}

// Return a 400 with the user-representation of the validation error as body.
// The error root cause is preserved for logging purposes.
pub(crate) fn e400<T: std::fmt::Debug + std::fmt::Display>(e: T) -> actix_web::Error
where
    T: Debug + Display + 'static,
{
    actix_web::error::ErrorBadRequest(e)
}

pub(crate) fn serialize_as_hex<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let hex_string = format!("0x{}", hex::encode(bytes));
    serializer.serialize_str(&hex_string)
}

pub(crate) fn deserialize_from_hex<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    struct HexVisitor;

    impl<'de> Visitor<'de> for HexVisitor {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a hex string with a 0x prefix")
        }

        fn visit_str<E>(self, value: &str) -> Result<Vec<u8>, E>
        where
            E: de::Error,
        {
            if value.starts_with("0x") || value.starts_with("0X") {
                hex::decode(&value[2..]).map_err(E::custom)
            } else {
                Err(E::custom("Expected a hex string with a 0x prefix"))
            }
        }
    }

    deserializer.deserialize_str(HexVisitor)
}
