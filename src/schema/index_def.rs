use std::io;
#[derive(Debug, Clone)]
pub struct IndexDef {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub root_page: u64,
}

impl IndexDef {
    pub fn new(name: impl Into<String>, columns: Vec<String>, unique: bool) -> Self {
        Self {
            name: name.into(),
            columns,
            unique,
            root_page: 0, // Will be set when index is created
        }
    }

    pub(crate) fn serialize(&self, buf: &mut Vec<u8>) {
        // Name
        let name_bytes = self.name.as_bytes();
        buf.push(name_bytes.len() as u8);
        buf.extend_from_slice(name_bytes);
        // Columns
        buf.push(self.columns.len() as u8);
        for col in &self.columns {
            let col_bytes = col.as_bytes();
            buf.push(col_bytes.len() as u8);
            buf.extend_from_slice(col_bytes);
        }
        // Unique flag
        buf.push(if self.unique { 1 } else { 0 });
        // Root page
        buf.extend_from_slice(&self.root_page.to_le_bytes());
    }

    pub(crate) fn deserialize(data: &[u8], offset: &mut usize) -> io::Result<Self> {
        let name_len = data[*offset] as usize;
        *offset += 1;
        let name = String::from_utf8(data[*offset..*offset + name_len].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        *offset += name_len;

        let col_count = data[*offset] as usize;
        *offset += 1;
        let mut columns = Vec::with_capacity(col_count);
        for _ in 0..col_count {
            let col_len = data[*offset] as usize;
            *offset += 1;
            let col = String::from_utf8(data[*offset..*offset + col_len].to_vec())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            *offset += col_len;
            columns.push(col);
        }

        let unique = data[*offset] != 0;
        *offset += 1;

        let root_page = u64::from_le_bytes(data[*offset..*offset + 8].try_into().unwrap());
        *offset += 8;

        Ok(IndexDef { name, columns, unique, root_page })
    }
}