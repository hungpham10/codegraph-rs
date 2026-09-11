use codegraph_graph::Element;
use serde::{Deserialize, Serialize};

/// Tag bits (top 8 bits of a `u64`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DocTag {
    #[default]
    Map,
    Arr,
    Field,
    Idx,
    Str,
    Num,
    Bool,
    Null,
    Root,
}

impl DocTag {
    fn bits(self) -> u64 {
        self as u64
    }
}

/// A structural token: 8 bytes total (8-bit tag + 56-bit payload).
///
/// Payload meanings by tag:
/// - `Field` → interned key id
/// - `Idx` → array slot (u32)
/// - `Str` / `Num` / `Bool` / `Null` → interned value/type id
/// - `Map` / `Arr` / `Root` → 0
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
pub struct DocToken(u64);

impl DocToken {
    pub const TAG_BITS: u64 = 0xFF;
    pub const PAYLOAD_MASK: u64 = 0x00FFFFFFFFFFFFFF;

    pub fn new(tag: DocTag, payload: u64) -> Self {
        Self((tag.bits() << 56) | (payload & Self::PAYLOAD_MASK))
    }

    pub fn tag(&self) -> DocTag {
        match (self.0 >> 56) as u8 {
            0 => DocTag::Map,
            1 => DocTag::Arr,
            2 => DocTag::Field,
            3 => DocTag::Idx,
            4 => DocTag::Str,
            5 => DocTag::Num,
            6 => DocTag::Bool,
            7 => DocTag::Null,
            8 => DocTag::Root,
            _ => DocTag::Map,
        }
    }

    pub fn payload(&self) -> u64 {
        self.0 & Self::PAYLOAD_MASK
    }

    pub fn field_key_id(&self) -> u64 {
        self.payload()
    }
    pub fn index_slot(&self) -> u32 {
        self.payload() as u32
    }
    pub fn value_id(&self) -> u64 {
        self.payload()
    }
}

impl Element for DocToken {
    fn encode(&self) -> Vec<u8> {
        self.0.to_be_bytes().to_vec()
    }

    fn decode(bytes: &[u8]) -> Self {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&bytes[..8.min(bytes.len())]);
        Self(u64::from_be_bytes(buf))
    }

    fn byte_size() -> usize {
        8
    }

    fn to_usize(&self) -> usize {
        self.0 as usize
    }
}

// Helper constructors
impl DocToken {
    pub fn map() -> Self {
        Self::new(DocTag::Map, 0)
    }
    pub fn arr() -> Self {
        Self::new(DocTag::Arr, 0)
    }
    pub fn field(key_id: u64) -> Self {
        Self::new(DocTag::Field, key_id)
    }
    pub fn idx(slot: u32) -> Self {
        Self::new(DocTag::Idx, slot as u64)
    }
    pub fn str(value_id: u64) -> Self {
        Self::new(DocTag::Str, value_id)
    }
    pub fn num(value_id: u64) -> Self {
        Self::new(DocTag::Num, value_id)
    }
    pub fn bool(value_id: u64) -> Self {
        Self::new(DocTag::Bool, value_id)
    }
    pub fn null() -> Self {
        Self::new(DocTag::Null, 0)
    }
    pub fn root() -> Self {
        Self::new(DocTag::Root, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let tok = DocToken::field(42);
        let bytes = tok.encode();
        let decoded = DocToken::decode(&bytes);
        assert_eq!(tok, decoded);
    }

    #[test]
    fn tag_payload_accessors() {
        let tok = DocToken::field(123);
        assert_eq!(tok.tag(), DocTag::Field);
        assert_eq!(tok.payload(), 123);
        assert_eq!(tok.field_key_id(), 123);
    }
}
