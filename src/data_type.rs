#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DataType {
    Null = 0,
    Bool = 1,
    UInt32 = 2,
    UInt64 = 3,
    Int32 = 4,
    Int64 = 5,
    Float64 = 6,
    Text = 7,
    Blob = 8,
    Timestamp = 9,
}

impl DataType {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(DataType::Null),
            1 => Some(DataType::Bool),
            2 => Some(DataType::UInt32),
            3 => Some(DataType::UInt64),
            4 => Some(DataType::Int32),
            5 => Some(DataType::Int64),
            6 => Some(DataType::Float64),
            7 => Some(DataType::Text),
            8 => Some(DataType::Blob),
            9 => Some(DataType::Timestamp),
            _ => None,
        }
    }

    fn fixed_size(&self) -> Option<usize> {
        match self {
            DataType::Null => Some(0),
            DataType::Bool => Some(1),
            DataType::Int32 | DataType::UInt32 => Some(4),
            DataType::Int64 | DataType::UInt64 | DataType::Float64 | DataType::Timestamp => Some(8),
            DataType::Text | DataType::Blob => None,
        }
    }
}