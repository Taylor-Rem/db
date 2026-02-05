use std::io;
const MAGIC: [u8; 4] = *b"BPDB";
const VERSION: u32 = 1;
pub const PAGE_SIZE: usize = 4096;
const HEADER_SIZE: usize = PAGE_SIZE;
#[derive(Debug)]
pub struct FileHeader {
    magic: [u8; 4],
    version: u32,
    page_size: u32,
    pub(crate) total_pages: u64,
    free_list_head: u64,
    pub(crate) schema_catalog_root: u64,
}

impl FileHeader {
    pub(crate) fn new() -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            page_size: PAGE_SIZE as u32,
            total_pages: 1, // Just the header page initially
            free_list_head: 0,
            schema_catalog_root: 0,
        }
    }

    pub(crate) fn serialize(&self) -> Vec<u8> {
        let mut buf = vec![0u8; PAGE_SIZE];
        buf[0..4].copy_from_slice(&self.magic);
        buf[4..8].copy_from_slice(&self.version.to_le_bytes());
        buf[8..12].copy_from_slice(&self.page_size.to_le_bytes());
        buf[12..20].copy_from_slice(&self.total_pages.to_le_bytes());
        buf[20..28].copy_from_slice(&self.free_list_head.to_le_bytes());
        buf[28..36].copy_from_slice(&self.schema_catalog_root.to_le_bytes());
        buf
    }

    pub(crate) fn deserialize(data: &[u8]) -> io::Result<Self> {
        if &data[0..4] != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid magic number"));
        }

        Ok(Self {
            magic: MAGIC,
            version: u32::from_le_bytes(data[4..8].try_into().unwrap()),
            page_size: u32::from_le_bytes(data[8..12].try_into().unwrap()),
            total_pages: u64::from_le_bytes(data[12..20].try_into().unwrap()),
            free_list_head: u64::from_le_bytes(data[20..28].try_into().unwrap()),
            schema_catalog_root: u64::from_le_bytes(data[28..36].try_into().unwrap()),
        })
    }
}