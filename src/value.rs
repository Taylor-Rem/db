use std::io;
pub use crate::data_type::DataType;


#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    UInt32(u32),
    UInt64(u64),
    Int32(i32),
    Int64(i64),
    Float64(f64),
    Text(String),
    Blob(Vec<u8>),
    Timestamp(i64),
}

impl Value {
    fn data_type(&self) -> DataType {
        match self {
            Value::Null => DataType::Null,
            Value::Bool(_) => DataType::Bool,
            Value::UInt32(_) => DataType::UInt32,
            Value::UInt64(_) => DataType::UInt64,
            Value::Int32(_) => DataType::Int32,
            Value::Int64(_) => DataType::Int64,
            Value::Float64(_) => DataType::Float64,
            Value::Text(_) => DataType::Text,
            Value::Blob(_) => DataType::Blob,
            Value::Timestamp(_) => DataType::Timestamp,
        }
    }

    pub fn serialize(&self, buf: &mut Vec<u8>) {
        match self {
            Value::Null => {}
            Value::Bool(b) => buf.push(if *b { 1 } else { 0 }),
            Value::UInt32(u) => buf.push(*u as u8),
            Value::UInt64(u) => buf.push(*u as u8),
            Value::Int32(n) => buf.extend_from_slice(&n.to_le_bytes()),
            Value::Int64(n) => buf.extend_from_slice(&n.to_le_bytes()),
            Value::Float64(f) => buf.extend_from_slice(&f.to_le_bytes()),
            Value::Text(s) => {
                let bytes = s.as_bytes();
                buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(bytes);
            }
            Value::Blob(data) => {
                buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
                buf.extend_from_slice(data);
            }
            Value::Timestamp(ts) => buf.extend_from_slice(&ts.to_le_bytes()),
        }
    }

    pub(crate) fn deserialize(dtype: DataType, data: &[u8], offset: &mut usize) -> io::Result<Self> {
        match dtype {
            DataType::Null => Ok(Value::Null),
            DataType::Bool => {
                let v = data.get(*offset).ok_or(io::Error::new(io::ErrorKind::UnexpectedEof, "bool"))?;
                *offset += 1;
                Ok(Value::Bool(*v != 0))
            }
            DataType::UInt32 => {
                let bytes: [u8; 4] = data[*offset..*offset + 4].try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "u32"))?;
                *offset += 4;
                Ok(Value::UInt32(u32::from_le_bytes(bytes)))
            }
            DataType::UInt64 => {
                let bytes: [u8; 8] = data[*offset..*offset + 8].try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "u64"))?;
                *offset += 8;
                Ok(Value::UInt64(u64::from_le_bytes(bytes)))
            }
            DataType::Int32 => {
                let bytes: [u8; 4] = data[*offset..*offset + 4].try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "i32"))?;
                *offset += 4;
                Ok(Value::Int32(i32::from_le_bytes(bytes)))
            }
            DataType::Int64 => {
                let bytes: [u8; 8] = data[*offset..*offset + 8].try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "i64"))?;
                *offset += 8;
                Ok(Value::Int64(i64::from_le_bytes(bytes)))
            }
            DataType::Float64 => {
                let bytes: [u8; 8] = data[*offset..*offset + 8].try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "f64"))?;
                *offset += 8;
                Ok(Value::Float64(f64::from_le_bytes(bytes)))
            }
            DataType::Text => {
                let len_bytes: [u8; 4] = data[*offset..*offset + 4].try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "text len"))?;
                let len = u32::from_le_bytes(len_bytes) as usize;
                *offset += 4;
                let s = String::from_utf8(data[*offset..*offset + len].to_vec())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                *offset += len;
                Ok(Value::Text(s))
            }
            DataType::Blob => {
                let len_bytes: [u8; 4] = data[*offset..*offset + 4].try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "blob len"))?;
                let len = u32::from_le_bytes(len_bytes) as usize;
                *offset += 4;
                let blob = data[*offset..*offset + len].to_vec();
                *offset += len;
                Ok(Value::Blob(blob))
            }
            DataType::Timestamp => {
                let bytes: [u8; 8] = data[*offset..*offset + 8].try_into()
                    .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "timestamp"))?;
                *offset += 8;
                Ok(Value::Timestamp(i64::from_le_bytes(bytes)))
            }
        }
    }

    /// Compare for B+ tree ordering (used as keys)
    fn cmp_key(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Less,
            (_, Value::Null) => Ordering::Greater,
            (Value::Int32(a), Value::Int32(b)) => a.cmp(b),
            (Value::Int64(a), Value::Int64(b)) => a.cmp(b),
            (Value::Text(a), Value::Text(b)) => a.cmp(b),
            (Value::Blob(a), Value::Blob(b)) => a.cmp(b),
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Float64(a), Value::Float64(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Value::Timestamp(a), Value::Timestamp(b)) => a.cmp(b),
            _ => Ordering::Equal, // Mixed types: treat as equal (shouldn't happen with schema)
        }
    }
}