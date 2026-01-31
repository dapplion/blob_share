use c_kzg::{BYTES_PER_G1_POINT, BYTES_PER_G2_POINT};
use serde::{
    de::{self, Deserializer, Visitor},
    Deserialize, Serialize,
};

/// Wrapper over a BLS G1 point's byte representation.
#[derive(Debug, Clone, PartialEq)]
struct G1Point([u8; BYTES_PER_G1_POINT]);

/// Wrapper over a BLS G2 point's byte representation.
#[derive(Debug, Clone, PartialEq)]
struct G2Point([u8; BYTES_PER_G2_POINT]);

/// Contains the trusted setup parameters that are required to instantiate a
/// `c_kzg::KzgSettings` object.
///
/// The serialize/deserialize implementations are written according to
/// the format specified in the the ethereum consensus specs trusted setup files.
///
/// See <https://github.com/ethereum/consensus-specs/blob/dev/presets/mainnet/trusted_setups/trusted_setup_4096.json>
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrustedSetup {
    #[serde(rename = "g1_lagrange")]
    g1_points: Vec<G1Point>,
    #[serde(rename = "g2_monomial")]
    g2_points: Vec<G2Point>,
}

impl TrustedSetup {
    pub fn g1_points(&self) -> Vec<[u8; BYTES_PER_G1_POINT]> {
        self.g1_points.iter().map(|p| p.0).collect()
    }

    pub fn g2_points(&self) -> Vec<[u8; BYTES_PER_G2_POINT]> {
        self.g2_points.iter().map(|p| p.0).collect()
    }
}

impl Serialize for G1Point {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let point = hex::encode(self.0);
        serializer.serialize_str(&point)
    }
}

impl Serialize for G2Point {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let point = hex::encode(self.0);
        serializer.serialize_str(&point)
    }
}

impl<'de> Deserialize<'de> for G1Point {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct G1PointVisitor;

        impl<'de> Visitor<'de> for G1PointVisitor {
            type Value = G1Point;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("A 48 byte hex encoded string")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let point = hex::decode(strip_prefix(v))
                    .map_err(|e| de::Error::custom(format!("Failed to decode G1 point: {}", e)))?;
                if point.len() != BYTES_PER_G1_POINT {
                    return Err(de::Error::custom(format!(
                        "G1 point has invalid length. Expected {} got {}",
                        BYTES_PER_G1_POINT,
                        point.len()
                    )));
                }
                let mut res = [0; BYTES_PER_G1_POINT];
                res.copy_from_slice(&point);
                Ok(G1Point(res))
            }
        }

        deserializer.deserialize_str(G1PointVisitor)
    }
}

impl<'de> Deserialize<'de> for G2Point {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct G2PointVisitor;

        impl<'de> Visitor<'de> for G2PointVisitor {
            type Value = G2Point;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("A 96 byte hex encoded string")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let point = hex::decode(strip_prefix(v))
                    .map_err(|e| de::Error::custom(format!("Failed to decode G2 point: {}", e)))?;
                if point.len() != BYTES_PER_G2_POINT {
                    return Err(de::Error::custom(format!(
                        "G2 point has invalid length. Expected {} got {}",
                        BYTES_PER_G2_POINT,
                        point.len()
                    )));
                }
                let mut res = [0; BYTES_PER_G2_POINT];
                res.copy_from_slice(&point);
                Ok(G2Point(res))
            }
        }

        deserializer.deserialize_str(G2PointVisitor)
    }
}

fn strip_prefix(s: &str) -> &str {
    if let Some(stripped) = s.strip_prefix("0x") {
        stripped
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use c_kzg::{BYTES_PER_G1_POINT, BYTES_PER_G2_POINT};

    // --- strip_prefix tests ---

    #[test]
    fn strip_prefix_removes_0x() {
        assert_eq!(strip_prefix("0xabcd"), "abcd");
    }

    #[test]
    fn strip_prefix_no_prefix() {
        assert_eq!(strip_prefix("abcd"), "abcd");
    }

    #[test]
    fn strip_prefix_empty_string() {
        assert_eq!(strip_prefix(""), "");
    }

    #[test]
    fn strip_prefix_only_0x() {
        assert_eq!(strip_prefix("0x"), "");
    }

    // --- G1Point serde tests ---

    #[test]
    fn g1_point_serialize_produces_hex_string() {
        let mut bytes = [0u8; BYTES_PER_G1_POINT];
        bytes[0] = 0xab;
        bytes[47] = 0xcd;
        let point = G1Point(bytes);
        let json = serde_json::to_string(&point).unwrap();
        // Should be a hex-encoded string (no 0x prefix in serialization)
        let hex_str: String = serde_json::from_str(&json).unwrap();
        assert_eq!(hex_str.len(), BYTES_PER_G1_POINT * 2);
        assert!(hex_str.starts_with("ab"));
        assert!(hex_str.ends_with("cd"));
    }

    #[test]
    fn g1_point_deserialize_from_hex_without_prefix() {
        let bytes = [0xffu8; BYTES_PER_G1_POINT];
        let hex_str = hex::encode(bytes);
        let json = format!("\"{}\"", hex_str);
        let point: G1Point = serde_json::from_str(&json).unwrap();
        assert_eq!(point.0, bytes);
    }

    #[test]
    fn g1_point_deserialize_from_hex_with_0x_prefix() {
        let bytes = [0x42u8; BYTES_PER_G1_POINT];
        let hex_str = format!("0x{}", hex::encode(bytes));
        let json = format!("\"{}\"", hex_str);
        let point: G1Point = serde_json::from_str(&json).unwrap();
        assert_eq!(point.0, bytes);
    }

    #[test]
    fn g1_point_roundtrip() {
        let mut bytes = [0u8; BYTES_PER_G1_POINT];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = i as u8;
        }
        let original = G1Point(bytes);
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: G1Point = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn g1_point_deserialize_wrong_length_fails() {
        // 47 bytes instead of 48
        let hex_str = hex::encode([0u8; BYTES_PER_G1_POINT - 1]);
        let json = format!("\"{}\"", hex_str);
        let result: Result<G1Point, _> = serde_json::from_str(&json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid length"), "error was: {}", err);
    }

    #[test]
    fn g1_point_deserialize_invalid_hex_fails() {
        let json = format!("\"{}\"", "zzzz".repeat(BYTES_PER_G1_POINT / 2));
        let result: Result<G1Point, _> = serde_json::from_str(&json);
        assert!(result.is_err());
    }

    // --- G2Point serde tests ---

    #[test]
    fn g2_point_serialize_produces_hex_string() {
        let mut bytes = [0u8; BYTES_PER_G2_POINT];
        bytes[0] = 0xde;
        bytes[95] = 0xef;
        let point = G2Point(bytes);
        let json = serde_json::to_string(&point).unwrap();
        let hex_str: String = serde_json::from_str(&json).unwrap();
        assert_eq!(hex_str.len(), BYTES_PER_G2_POINT * 2);
        assert!(hex_str.starts_with("de"));
        assert!(hex_str.ends_with("ef"));
    }

    #[test]
    fn g2_point_deserialize_from_hex_without_prefix() {
        let bytes = [0xaau8; BYTES_PER_G2_POINT];
        let hex_str = hex::encode(bytes);
        let json = format!("\"{}\"", hex_str);
        let point: G2Point = serde_json::from_str(&json).unwrap();
        assert_eq!(point.0, bytes);
    }

    #[test]
    fn g2_point_deserialize_from_hex_with_0x_prefix() {
        let bytes = [0x11u8; BYTES_PER_G2_POINT];
        let hex_str = format!("0x{}", hex::encode(bytes));
        let json = format!("\"{}\"", hex_str);
        let point: G2Point = serde_json::from_str(&json).unwrap();
        assert_eq!(point.0, bytes);
    }

    #[test]
    fn g2_point_roundtrip() {
        let mut bytes = [0u8; BYTES_PER_G2_POINT];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = i as u8;
        }
        let original = G2Point(bytes);
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: G2Point = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn g2_point_deserialize_wrong_length_fails() {
        // 95 bytes instead of 96
        let hex_str = hex::encode([0u8; BYTES_PER_G2_POINT - 1]);
        let json = format!("\"{}\"", hex_str);
        let result: Result<G2Point, _> = serde_json::from_str(&json);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid length"), "error was: {}", err);
    }

    #[test]
    fn g2_point_deserialize_invalid_hex_fails() {
        let json = format!("\"{}\"", "xxxx".repeat(BYTES_PER_G2_POINT / 2));
        let result: Result<G2Point, _> = serde_json::from_str(&json);
        assert!(result.is_err());
    }

    // --- TrustedSetup serde tests ---

    #[test]
    fn trusted_setup_roundtrip() {
        let g1 = G1Point([0x01; BYTES_PER_G1_POINT]);
        let g2 = G2Point([0x02; BYTES_PER_G2_POINT]);
        let setup = TrustedSetup {
            g1_points: vec![g1.clone()],
            g2_points: vec![g2.clone()],
        };
        let json = serde_json::to_string(&setup).unwrap();
        let deserialized: TrustedSetup = serde_json::from_str(&json).unwrap();
        assert_eq!(setup, deserialized);
    }

    #[test]
    fn trusted_setup_uses_renamed_fields() {
        let setup = TrustedSetup {
            g1_points: vec![G1Point([0; BYTES_PER_G1_POINT])],
            g2_points: vec![G2Point([0; BYTES_PER_G2_POINT])],
        };
        let json = serde_json::to_string(&setup).unwrap();
        // JSON should use "g1_lagrange" and "g2_monomial" field names
        assert!(json.contains("g1_lagrange"), "json was: {}", json);
        assert!(json.contains("g2_monomial"), "json was: {}", json);
        assert!(!json.contains("g1_points"), "json was: {}", json);
        assert!(!json.contains("g2_points"), "json was: {}", json);
    }

    #[test]
    fn trusted_setup_empty_points() {
        let setup = TrustedSetup {
            g1_points: vec![],
            g2_points: vec![],
        };
        let json = serde_json::to_string(&setup).unwrap();
        let deserialized: TrustedSetup = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.g1_points().len(), 0);
        assert_eq!(deserialized.g2_points().len(), 0);
    }

    #[test]
    fn trusted_setup_multiple_points() {
        let g1a = G1Point([0x01; BYTES_PER_G1_POINT]);
        let g1b = G1Point([0x02; BYTES_PER_G1_POINT]);
        let g2a = G2Point([0x0a; BYTES_PER_G2_POINT]);
        let g2b = G2Point([0x0b; BYTES_PER_G2_POINT]);
        let setup = TrustedSetup {
            g1_points: vec![g1a, g1b],
            g2_points: vec![g2a, g2b],
        };
        let points_g1 = setup.g1_points();
        let points_g2 = setup.g2_points();
        assert_eq!(points_g1.len(), 2);
        assert_eq!(points_g2.len(), 2);
        assert_eq!(points_g1[0], [0x01; BYTES_PER_G1_POINT]);
        assert_eq!(points_g1[1], [0x02; BYTES_PER_G1_POINT]);
        assert_eq!(points_g2[0], [0x0a; BYTES_PER_G2_POINT]);
        assert_eq!(points_g2[1], [0x0b; BYTES_PER_G2_POINT]);
    }

    #[test]
    fn trusted_setup_deserialize_from_consensus_format() {
        // Simulate the format used in ethereum consensus specs trusted_setup JSON
        let g1_hex = hex::encode([0xaa; BYTES_PER_G1_POINT]);
        let g2_hex = format!("0x{}", hex::encode([0xbb; BYTES_PER_G2_POINT]));
        let json = format!(
            r#"{{"g1_lagrange":["{}"],"g2_monomial":["{}"]}}"#,
            g1_hex, g2_hex
        );
        let setup: TrustedSetup = serde_json::from_str(&json).unwrap();
        assert_eq!(setup.g1_points().len(), 1);
        assert_eq!(setup.g2_points().len(), 1);
        assert_eq!(setup.g1_points()[0], [0xaa; BYTES_PER_G1_POINT]);
        assert_eq!(setup.g2_points()[0], [0xbb; BYTES_PER_G2_POINT]);
    }

    #[test]
    fn real_trusted_setup_deserializes() {
        // Verify the actual embedded trusted setup JSON can be deserialized
        let setup: TrustedSetup = serde_json::from_slice(crate::TRUSTED_SETUP_BYTES).unwrap();
        assert!(
            !setup.g1_points().is_empty(),
            "trusted setup should have g1 points"
        );
        assert!(
            !setup.g2_points().is_empty(),
            "trusted setup should have g2 points"
        );
    }
}
