#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DataType {
    Null = 0,
    Bool = 1,
    Int32 = 2,
    Int64 = 3,
    Float64 = 4,
    Text = 5,      // Variable length, stored as length-prefixed
    Blob = 6,      // Variable length binary
    Timestamp = 7, // i64 unix millis
}

impl DataType {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(DataType::Null),
            1 => Some(DataType::Bool),
            2 => Some(DataType::Int32),
            3 => Some(DataType::Int64),
            4 => Some(DataType::Float64),
            5 => Some(DataType::Text),
            6 => Some(DataType::Blob),
            7 => Some(DataType::Timestamp),
            _ => None,
        }
    }

    /// Fixed size in bytes, or None for variable-length types
    fn fixed_size(&self) -> Option<usize> {
        match self {
            DataType::Null => Some(0),
            DataType::Bool => Some(1),
            DataType::Int32 => Some(4),
            DataType::Int64 | DataType::Float64 | DataType::Timestamp => Some(8),
            DataType::Text | DataType::Blob => None,
        }
    }
}