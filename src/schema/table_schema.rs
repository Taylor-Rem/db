use std::io;
use super::column::Column;
use super::index_def::IndexDef;
#[derive(Debug, Clone)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<Column>,
    pub primary_key: Vec<String>,
    pub indexes: Vec<IndexDef>,
    pub root_page: u64,
    pub row_count: u64,
    pub auto_increment: u64,
}

impl TableSchema {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            columns: Vec::new(),
            primary_key: Vec::new(),
            indexes: Vec::new(),
            root_page: 0,
            row_count: 0,
            auto_increment: 1,
        }
    }

    pub fn column(mut self, col: Column) -> Self {
        self.columns.push(col);
        self
    }

    pub fn primary_key(mut self, columns: &[&str]) -> Self {
        self.primary_key = columns.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn get_column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn get_column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    pub fn index(mut self, idx: IndexDef) -> Self {
        self.indexes.push(idx);
        self
    }

    pub fn get_index(&self, name: &str) -> Option<&IndexDef> {
        self.indexes.iter().find(|idx| idx.name == name)
    }

    pub(crate) fn serialize(&self, buf: &mut Vec<u8>) {
        // Table name
        let name_bytes = self.name.as_bytes();
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(name_bytes);

        // Columns
        buf.extend_from_slice(&(self.columns.len() as u16).to_le_bytes());
        for col in &self.columns {
            col.serialize(buf);
        }

        // Primary key columns
        buf.push(self.primary_key.len() as u8);
        for pk in &self.primary_key {
            let pk_bytes = pk.as_bytes();
            buf.push(pk_bytes.len() as u8);
            buf.extend_from_slice(pk_bytes);
        }

        // Indexes
        buf.extend_from_slice(&(self.indexes.len() as u16).to_le_bytes());
        for idx in &self.indexes {
            idx.serialize(buf);
        }

        // Root page, row count, auto increment
        buf.extend_from_slice(&self.root_page.to_le_bytes());
        buf.extend_from_slice(&self.row_count.to_le_bytes());
        buf.extend_from_slice(&self.auto_increment.to_le_bytes());
    }

    pub(crate) fn deserialize(data: &[u8]) -> io::Result<Self> {
        let mut offset = 0;

        let name_len = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;
        let name = String::from_utf8(data[offset..offset + name_len].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        offset += name_len;

        let col_count = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;
        let mut columns = Vec::with_capacity(col_count);
        for _ in 0..col_count {
            columns.push(Column::deserialize(data, &mut offset)?);
        }

        let pk_count = data[offset] as usize;
        offset += 1;
        let mut primary_key = Vec::with_capacity(pk_count);
        for _ in 0..pk_count {
            let pk_len = data[offset] as usize;
            offset += 1;
            let pk = String::from_utf8(data[offset..offset + pk_len].to_vec())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            offset += pk_len;
            primary_key.push(pk);
        }

        let idx_count = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
        offset += 2;
        let mut indexes = Vec::with_capacity(idx_count);
        for _ in 0..idx_count {
            indexes.push(IndexDef::deserialize(data, &mut offset)?);
        }

        let root_page = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        offset += 8;
        let row_count = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        offset += 8;
        let auto_increment = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());

        Ok(TableSchema {
            name,
            columns,
            primary_key,
            indexes,
            root_page,
            row_count,
            auto_increment,
        })
    }
}