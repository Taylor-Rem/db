use std::io;
use crate::storage::file_header::PAGE_SIZE;
pub(crate) const NODE_INTERNAL: u8 = 0x01;
const BTREE_ORDER: usize = 128;
#[derive(Debug)]
pub(crate) struct InternalNode {
    pub(crate) keys: Vec<Vec<u8>>,      // Serialized key values
    pub(crate) children: Vec<u64>,      // Page numbers of children
}

impl InternalNode {
    pub(crate) fn new() -> Self {
        Self {
            keys: Vec::new(),
            children: Vec::new(),
        }
    }

    pub(crate) fn serialize(&self) -> Vec<u8> {
        let mut buf = vec![0u8; PAGE_SIZE];
        buf[0] = NODE_INTERNAL;

        let mut offset = 1;

        // Number of keys
        let key_count = self.keys.len() as u16;
        buf[offset..offset + 2].copy_from_slice(&key_count.to_le_bytes());
        offset += 2;

        // Keys with their lengths
        for key in &self.keys {
            let key_len = key.len() as u16;
            buf[offset..offset + 2].copy_from_slice(&key_len.to_le_bytes());
            offset += 2;
            buf[offset..offset + key.len()].copy_from_slice(key);
            offset += key.len();
        }

        // Children (key_count + 1 children)
        for child in &self.children {
            buf[offset..offset + 8].copy_from_slice(&child.to_le_bytes());
            offset += 8;
        }

        buf
    }

    pub(crate) fn deserialize(data: &[u8]) -> io::Result<Self> {
        if data[0] != NODE_INTERNAL {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not an internal node"));
        }

        let mut offset = 1;
        let key_count = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;

        let mut keys = Vec::with_capacity(key_count);
        for _ in 0..key_count {
            let key_len = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
            offset += 2;
            keys.push(data[offset..offset + key_len].to_vec());
            offset += key_len;
        }

        let child_count = key_count + 1;
        let mut children = Vec::with_capacity(child_count);
        for _ in 0..child_count {
            let child = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
            offset += 8;
            children.push(child);
        }

        Ok(InternalNode { keys, children })
    }

    pub(crate) fn is_full(&self) -> bool {
        self.keys.len() >= BTREE_ORDER - 1
    }
}

