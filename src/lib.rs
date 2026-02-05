// Core types
pub mod data_type;
pub mod value;

// Schema definitions
pub mod schema;

// Storage layer
pub mod storage;

// B+ tree implementation
pub mod btree;

// Row representation
pub mod row;

// Database engine
pub mod database;

// Re-exports for convenience
pub use data_type::DataType;
pub use value::Value;
pub use row::Row;
pub use database::Database;
pub use schema::{Column, TableSchema, TableBuilder, IndexDef};
