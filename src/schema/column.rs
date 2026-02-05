use std::io;
use crate::{data_type::DataType, value::Value};

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub default: Option<Value>,
    pub auto_increment: bool,  // NEW: Auto-increment flag
}

impl Column {
    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Self {
            name: name.into(),
            data_type,
            nullable: true,
            default: None,
            auto_increment: false,  // NEW: Default to false
        }
    }

    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    pub fn default(mut self, value: Value) -> Self {
        self.default = Some(value);
        self
    }

    // NEW: Builder method for auto-increment
    pub fn auto_increment(mut self) -> Self {
        self.auto_increment = true;
        self.nullable = false;  // Auto-increment columns can't be null
        self
    }

    pub(crate) fn serialize(&self, buf: &mut Vec<u8>) {
        // Name: length-prefixed string
        let name_bytes = self.name.as_bytes();
        buf.push(name_bytes.len() as u8);
        buf.extend_from_slice(name_bytes);
        // Type
        buf.push(self.data_type as u8);
        // Flags: bit 0 = nullable, bit 1 = auto_increment (NEW)
        let flags = (if self.nullable { 1 } else { 0 })
            | (if self.auto_increment { 2 } else { 0 });
        buf.push(flags);
        // Default: has_default byte + serialized value
        match &self.default {
            Some(v) => {
                buf.push(1);
                v.serialize(buf);
            }
            None => buf.push(0),
        }
    }

    pub(crate) fn deserialize(data: &[u8], offset: &mut usize) -> io::Result<Self> {
        let name_len = data[*offset] as usize;
        *offset += 1;
        let name = String::from_utf8(data[*offset..*offset + name_len].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        *offset += name_len;

        let data_type = DataType::from_byte(data[*offset])
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unknown data type"))?;
        *offset += 1;

        // NEW: Parse flags
        let flags = data[*offset];
        let nullable = (flags & 1) != 0;
        let auto_increment = (flags & 2) != 0;
        *offset += 1;

        let has_default = data[*offset] != 0;
        *offset += 1;

        let default = if has_default {
            Some(Value::deserialize(data_type, data, offset)?)
        } else {
            None
        };

        Ok(Column {
            name,
            data_type,
            nullable,
            default,
            auto_increment,
        })
    }
}