use std::io;
use crate::file_header::PAGE_SIZE;
pub(crate) const NODE_LEAF: u8 = 0x02;
#[derive(Debug)]
pub(crate) struct LeafNode {
    pub(crate) keys: Vec<Vec<u8>>,      // Serialized primary key values
    pub(crate) values: Vec<Vec<u8>>,    // Serialized row data
    pub(crate) next_leaf: u64,          // Page number of next leaf (0 if none)
    pub(crate) prev_leaf: u64,          // Page number of previous leaf (0 if none)
}

impl LeafNode {
    pub(crate) fn new() -> Self {
        Self {
            keys: Vec::new(),
            values: Vec::new(),
            next_leaf: 0,
            prev_leaf: 0,
        }
    }

    pub(crate) fn serialize(&self) -> Vec<u8> {
        let mut buf = vec![0u8; PAGE_SIZE];
        buf[0] = NODE_LEAF;

        let mut offset = 1;

        // Next/prev leaf pointers
        buf[offset..offset + 8].copy_from_slice(&self.next_leaf.to_le_bytes());
        offset += 8;
        buf[offset..offset + 8].copy_from_slice(&self.prev_leaf.to_le_bytes());
        offset += 8;

        // Number of entries
        let entry_count = self.keys.len() as u16;
        buf[offset..offset + 2].copy_from_slice(&entry_count.to_le_bytes());
        offset += 2;

        // Entries: key_len + key + value_len + value
        for (key, value) in self.keys.iter().zip(self.values.iter()) {
            let key_len = key.len() as u16;
            buf[offset..offset + 2].copy_from_slice(&key_len.to_le_bytes());
            offset += 2;
            buf[offset..offset + key.len()].copy_from_slice(key);
            offset += key.len();

            let value_len = value.len() as u16;
            buf[offset..offset + 2].copy_from_slice(&value_len.to_le_bytes());
            offset += 2;
            buf[offset..offset + value.len()].copy_from_slice(value);
            offset += value.len();
        }

        buf
    }

    pub(crate) fn deserialize(data: &[u8]) -> io::Result<Self> {
        if data[0] != NODE_LEAF {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a leaf node"));
        }

        let mut offset = 1;

        let next_leaf = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        offset += 8;
        let prev_leaf = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        offset += 8;

        let entry_count = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;

        let mut keys = Vec::with_capacity(entry_count);
        let mut values = Vec::with_capacity(entry_count);

        for _ in 0..entry_count {
            let key_len = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
            offset += 2;
            keys.push(data[offset..offset + key_len].to_vec());
            offset += key_len;

            let value_len = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
            offset += 2;
            values.push(data[offset..offset + value_len].to_vec());
            offset += value_len;
        }

        Ok(LeafNode { keys, values, next_leaf, prev_leaf })
    }

    pub(crate) fn is_full(&self) -> bool {
        // Estimate: each entry ~100 bytes average, leave room for overhead
        self.keys.len() >= 30
    }

    pub(crate) fn find_key_position(&self, key: &[u8]) -> usize {
        self.keys.iter().position(|k| k.as_slice() >= key).unwrap_or(self.keys.len())
    }
}