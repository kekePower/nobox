//! Base64 for binary payloads on a JSON wire.
//!
//! Pixels are the only binary payload this protocol carries, and a JSON array
//! of numbers would multiply them severalfold. A dependency for one alphabet
//! is not worth taking, so the encoding lives here.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD: u8 = b'=';

/// Encodes bytes as standard padded base64.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map_or(0, u32::from);
        let third = chunk.get(2).copied().map_or(0, u32::from);
        let block = (first << 16) | (second << 8) | third;
        encoded.push(char::from(ALPHABET[((block >> 18) & 0x3f) as usize]));
        encoded.push(char::from(ALPHABET[((block >> 12) & 0x3f) as usize]));
        encoded.push(if chunk.len() > 1 {
            char::from(ALPHABET[((block >> 6) & 0x3f) as usize])
        } else {
            char::from(PAD)
        });
        encoded.push(if chunk.len() > 2 {
            char::from(ALPHABET[(block & 0x3f) as usize])
        } else {
            char::from(PAD)
        });
    }
    encoded
}

const fn value_of(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some((byte - b'A') as u32),
        b'a'..=b'z' => Some((byte - b'a') as u32 + 26),
        b'0'..=b'9' => Some((byte - b'0') as u32 + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes standard padded base64.
///
/// # Errors
///
/// Returns [`DecodeError`] for a wrong length, an unknown symbol, or padding
/// anywhere but at the end.
pub fn decode(text: &str) -> Result<Vec<u8>, DecodeError> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(DecodeError::Length);
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let last = index == bytes.len() / 4 - 1;
        let padding = chunk.iter().filter(|byte| **byte == PAD).count();
        if padding > 0 && (!last || padding > 2 || chunk[1] == PAD) {
            return Err(DecodeError::Padding);
        }
        let mut block = 0_u32;
        for byte in chunk {
            let value = if *byte == PAD {
                0
            } else {
                value_of(*byte).ok_or(DecodeError::Symbol)?
            };
            block = (block << 6) | value;
        }
        decoded.push(((block >> 16) & 0xff) as u8);
        if padding < 2 {
            decoded.push(((block >> 8) & 0xff) as u8);
        }
        if padding < 1 {
            decoded.push((block & 0xff) as u8);
        }
    }
    Ok(decoded)
}

/// Why base64 decoding failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// The input length was not a multiple of four.
    Length,
    /// The input contained a symbol outside the alphabet.
    Symbol,
    /// Padding appeared somewhere other than the end.
    Padding,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Length => "base64 length is not a multiple of four",
            Self::Symbol => "base64 contains an unknown symbol",
            Self::Padding => "base64 padding is misplaced",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for DecodeError {}

/// Bytes that travel as a base64 string.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Base64Bytes(Vec<u8>);

impl Base64Bytes {
    /// Wraps bytes for transport.
    #[must_use]
    pub const fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Returns the bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Returns the wrapped bytes.
    #[must_use]
    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }

    /// Returns how many bytes are carried.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether nothing is carried.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Vec<u8>> for Base64Bytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl Serialize for Base64Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Base64Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        decode(&text).map(Self).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{Base64Bytes, DecodeError, decode, encode};

    #[test]
    fn known_vectors_match_the_standard_alphabet() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn every_byte_survives_a_round_trip() {
        let bytes: Vec<u8> = (0..=255).collect();
        let decoded = decode(&encode(&bytes)).expect("decodes");
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn malformed_input_is_rejected() {
        assert_eq!(decode("Zg="), Err(DecodeError::Length));
        assert_eq!(decode("Zg$="), Err(DecodeError::Symbol));
        assert_eq!(decode("Z==="), Err(DecodeError::Padding));
        assert_eq!(decode("Zg==Zg=="), Err(DecodeError::Padding));
    }

    #[test]
    fn wrapped_bytes_travel_as_a_json_string() {
        let value = Base64Bytes::new(b"foobar".to_vec());
        let encoded = serde_json::to_string(&value).expect("encodes");
        assert_eq!(encoded, "\"Zm9vYmFy\"");
        let decoded: Base64Bytes = serde_json::from_str(&encoded).expect("decodes");
        assert_eq!(decoded, value);
        assert_eq!(decoded.len(), 6);
        assert!(!decoded.is_empty());
        assert_eq!(decoded.as_slice(), b"foobar");
        assert_eq!(decoded.into_inner(), b"foobar".to_vec());
    }

    #[test]
    fn invalid_json_payloads_fail_to_decode() {
        assert!(serde_json::from_str::<Base64Bytes>("\"not base64!\"").is_err());
    }
}
