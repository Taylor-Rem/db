use crate::{ table_schema::TableSchema, data_type::DataType, column::Column, value::Value };
pub struct TableBuilder {
    schema: TableSchema,
}

impl TableBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            schema: TableSchema::new(name),
        }
    }

    pub fn column(mut self, name: impl Into<String>, dtype: DataType) -> Self {
        self.schema.columns.push(Column::new(name, dtype));
        self
    }

    pub fn column_not_null(mut self, name: impl Into<String>, dtype: DataType) -> Self {
        self.schema.columns.push(Column::new(name, dtype).not_null());
        self
    }

    pub fn column_with_default(mut self, name: impl Into<String>, dtype: DataType, default: Value) -> Self {
        self.schema.columns.push(Column::new(name, dtype).default(default));
        self
    }

    pub fn primary_key(mut self, columns: &[&str]) -> Self {
        self.schema.primary_key = columns.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn build(self) -> TableSchema {
        self.schema
    }
}