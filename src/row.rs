use std::io;
use crate::value::Value;
use crate::schema::table_schema::TableSchema;

#[derive(Debug, Clone)]
pub struct Row {
    pub values: Vec<Value>,
}

impl Row {
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    pub fn get(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }

    pub(crate) fn serialize(&self, schema: &TableSchema) -> Vec<u8> {
        let mut buf = Vec::new();

        // Null bitmap: 1 bit per column
        let bitmap_bytes = (schema.columns.len() + 7) / 8;
        let mut null_bitmap = vec![0u8; bitmap_bytes];
        for (i, v) in self.values.iter().enumerate() {
            if matches!(v, Value::Null) {
                null_bitmap[i / 8] |= 1 << (i % 8);
            }
        }
        buf.extend_from_slice(&null_bitmap);

        // Serialize non-null values
        for v in self.values.iter() {
            if !matches!(v, Value::Null) {
                v.serialize(&mut buf);
            }
        }

        buf
    }

    pub(crate) fn deserialize(data: &[u8], schema: &TableSchema) -> io::Result<Self> {
        let mut offset = 0;

        // Read null bitmap
        let bitmap_bytes = (schema.columns.len() + 7) / 8;
        let null_bitmap = &data[offset..offset + bitmap_bytes];
        offset += bitmap_bytes;

        let mut values = Vec::with_capacity(schema.columns.len());
        for (i, col) in schema.columns.iter().enumerate() {
            let is_null = (null_bitmap[i / 8] >> (i % 8)) & 1 == 1;
            if is_null {
                values.push(Value::Null);
            } else {
                values.push(Value::deserialize(col.data_type, data, &mut offset)?);
            }
        }

        Ok(Row { values })
    }
}