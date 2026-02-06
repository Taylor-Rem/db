use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

use crate::btree::{InternalNode, LeafNode, NODE_INTERNAL, NODE_LEAF};
use crate::row::Row;
use crate::schema::{Column, IndexDef, TableSchema};
use crate::storage::{FileHeader, PageCache, PageManager, Page, PAGE_SIZE};
use crate::value::Value;
use crate::data_type::DataType;

/// Default page cache size (number of pages)
const DEFAULT_CACHE_SIZE: usize = 256;

pub struct Database {
    page_manager: PageManager,
    cache: PageCache,
    header: FileHeader,
    schemas: HashMap<String, TableSchema>,
}

#[derive(Debug, Clone)]
pub struct JoinedRow {
    /// Values from the left table
    pub left: Row,
    /// Values from the right table
    pub right: Row,
    /// For convenience: flattened values in order [left..., right...]
    pub all_values: Vec<Value>,
}

impl JoinedRow {
    fn new(left: Row, right: Row) -> Self {
        let mut all_values = left.values.clone();
        all_values.extend(right.values.clone());
        Self { left, right, all_values }
    }
}

/// Type of join to perform
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinType {
    Inner,
    LeftOuter,
}

impl Database {
    /// Create a new database file
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;

        let header = FileHeader::new();
        let mut page_manager = PageManager::new(file, 1);

        // Write the header page
        let mut header_page = Page::new(0);
        header_page.as_bytes_mut().copy_from_slice(&header.serialize());
        page_manager.write_page(&header_page)?;
        page_manager.sync()?;

        Ok(Self {
            page_manager,
            cache: PageCache::new(DEFAULT_CACHE_SIZE),
            header,
            schemas: HashMap::new(),
        })
    }

    /// Open an existing database file
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;

        // We don't know total_pages yet, use a temporary large value
        let mut page_manager = PageManager::new(file, u64::MAX);

        // Read header
        let header_page = page_manager.read_page(0)?;
        let header = FileHeader::deserialize(header_page.as_bytes())?;

        // Now create the real page manager with correct total_pages
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let page_manager = PageManager::new(file, header.total_pages);

        let mut db = Self {
            page_manager,
            cache: PageCache::new(DEFAULT_CACHE_SIZE),
            header,
            schemas: HashMap::new(),
        };

        // Load schema catalog
        db.load_schema_catalog()?;

        Ok(db)
    }

    /// Create with a custom cache size
    pub fn with_cache_size(mut self, size: usize) -> Self {
        self.cache = PageCache::new(size);
        self
    }

    // -------------------------------------------------------------------------
    // Page Management (now using PageManager and PageCache)
    // -------------------------------------------------------------------------

    fn read_page(&mut self, page_num: u64) -> io::Result<Vec<u8>> {
        // Check cache first
        if let Some(page) = self.cache.get(page_num) {
            return Ok(page.as_bytes().to_vec());
        }

        // Read from disk
        let page = self.page_manager.read_page(page_num)?;
        let data = page.as_bytes().to_vec();

        // Add to cache, flush evicted page if dirty
        if let Some(evicted) = self.cache.insert(page) {
            if evicted.dirty {
                self.page_manager.write_page(&evicted)?;
            }
        }

        Ok(data)
    }

    fn write_page(&mut self, page_num: u64, data: &[u8]) -> io::Result<()> {
        // Create page and mark as dirty
        let mut page = Page::from_data(page_num, data.to_vec());
        page.mark_dirty();

        // Insert into cache
        if let Some(evicted) = self.cache.insert(page) {
            if evicted.dirty {
                self.page_manager.write_page(&evicted)?;
            }
        }

        Ok(())
    }

    fn allocate_page(&mut self) -> io::Result<u64> {
        // TODO: Use free list for reuse
        let page_num = self.header.total_pages;
        self.header.total_pages += 1;

        // Initialize empty page
        let empty = vec![0u8; PAGE_SIZE];
        self.write_page(page_num, &empty)?;

        // Update header on disk
        self.flush_header()?;

        Ok(page_num)
    }

    fn flush_header(&mut self) -> io::Result<()> {
        let header_data = self.header.serialize();
        // Write header directly to disk (bypass cache for header)
        let mut header_page = Page::from_data(0, header_data);
        header_page.mark_dirty();
        self.page_manager.write_page(&header_page)
    }

    /// Flush all dirty pages to disk
    fn flush_cache(&mut self) -> io::Result<()> {
        // Collect dirty pages to avoid borrow issues
        let dirty_pages: Vec<Page> = self.cache.dirty_pages().cloned().collect();

        for page in dirty_pages {
            self.page_manager.write_page(&page)?;
        }

        self.cache.clear_dirty_flags();
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Schema Catalog Management
    // -------------------------------------------------------------------------

    fn load_schema_catalog(&mut self) -> io::Result<()> {
        if self.header.schema_catalog_root == 0 {
            return Ok(()); // No tables yet
        }

        let schemas = self.scan_btree(self.header.schema_catalog_root)?;
        for (key, value) in schemas {
            let name = String::from_utf8(key)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let schema = TableSchema::deserialize(&value)?;
            self.schemas.insert(name, schema);
        }

        Ok(())
    }

    fn save_schema(&mut self, schema: &TableSchema) -> io::Result<()> {
        let key = schema.name.as_bytes().to_vec();
        let mut value = Vec::new();
        schema.serialize(&mut value);

        if self.header.schema_catalog_root == 0 {
            // Create the schema catalog B+ tree
            let root_page = self.allocate_page()?;
            let mut leaf = LeafNode::new();
            leaf.keys.push(key);
            leaf.values.push(value);
            self.write_page(root_page, &leaf.serialize())?;
            self.header.schema_catalog_root = root_page;
            self.flush_header()?;
        } else {
            // Insert into existing catalog
            self.btree_insert(self.header.schema_catalog_root, key, value)?;
        }

        Ok(())
    }

    // -------------------------------------------------------------------------
    // Table Management API
    // -------------------------------------------------------------------------

    /// Create a new table with the given schema
    pub fn create_table(&mut self, mut schema: TableSchema) -> io::Result<()> {
        if self.schemas.contains_key(&schema.name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("table '{}' already exists", schema.name),
            ));
        }

        // Allocate root page for the table's B+ tree
        let root_page = self.allocate_page()?;

        // Initialize as empty leaf node
        let leaf = LeafNode::new();
        self.write_page(root_page, &leaf.serialize())?;

        schema.root_page = root_page;

        // Save to schema catalog
        self.save_schema(&schema)?;
        self.schemas.insert(schema.name.clone(), schema);

        Ok(())
    }

    /// Drop a table
    pub fn drop_table(&mut self, name: &str) -> io::Result<()> {
        if !self.schemas.contains_key(name) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("table '{}' does not exist", name),
            ));
        }

        // TODO: Free all pages used by the table
        // For now, just remove from catalog

        self.schemas.remove(name);

        // Remove from schema catalog B+ tree
        let key = name.as_bytes().to_vec();
        self.btree_delete(self.header.schema_catalog_root, &key)?;

        Ok(())
    }

    /// Get a table's schema
    pub fn get_schema(&self, name: &str) -> Option<&TableSchema> {
        self.schemas.get(name)
    }

    /// List all tables
    pub fn list_tables(&self) -> Vec<&str> {
        self.schemas.keys().map(|s| s.as_str()).collect()
    }

    /// Alter table: add a column
    pub fn add_column(&mut self, table_name: &str, column: Column) -> io::Result<()> {
        let schema = self.schemas.get_mut(table_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?;

        if schema.get_column(&column.name).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("column '{}' already exists", column.name),
            ));
        }

        schema.columns.push(column);
        let schema_clone = schema.clone();
        self.save_schema(&schema_clone)?;

        Ok(())
    }

    // -------------------------------------------------------------------------
    // Row Operations
    // -------------------------------------------------------------------------

    /// Insert a row into a table
    pub fn insert(&mut self, table_name: &str, mut row: Row) -> io::Result<()> {
        let schema = self.schemas.get(table_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?
            .clone();

        // Validate row matches schema
        if row.values.len() != schema.columns.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("expected {} columns, got {}", schema.columns.len(), row.values.len()),
            ));
        }

        // NEW: Handle auto-increment columns
        let mut schema_modified = false;
        let mut schema_mut = schema.clone();

        for (i, column) in schema.columns.iter().enumerate() {
            if column.auto_increment {
                // Check if value is Null (needs auto-generation)
                if matches!(row.values[i], Value::Null) {
                    let next_id = schema_mut.auto_increment;

                    // Generate value based on column type
                    row.values[i] = match column.data_type {
                        DataType::UInt32 => {
                            if next_id > u32::MAX as u64 {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidInput,
                                    "auto_increment value exceeds UInt32 range"
                                ));
                            }
                            Value::UInt32(next_id as u32)
                        }
                        DataType::UInt64 => {
                            Value::UInt64(next_id as u64)
                        }
                        DataType::Int32 => {
                            if next_id > i32::MAX as u64 {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidInput,
                                    "auto_increment value exceeds Int32 range"
                                ));
                            }
                            Value::Int32(next_id as i32)
                        }
                        DataType::Int64 => Value::Int64(next_id as i64),
                        _ => return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "auto_increment only supports Int32 and Int64 types"
                        )),
                    };

                    schema_mut.auto_increment = next_id + 1;
                    schema_modified = true;
                }
            }
        }

        // Build the primary key
        let pk_key = self.build_pk_key(&schema_mut, &row)?;
        let value = row.serialize(&schema_mut);

        // Check unique constraints on indexes and insert into indexes
        for index in &schema_mut.indexes {
            if index.unique {
                let prefix = self.build_index_key_prefix(&schema_mut, index, &row)?;
                let existing = self.btree_range_scan_prefix(index.root_page, &prefix)?;
                if !existing.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("duplicate key violates unique constraint on index '{}'", index.name),
                    ));
                }
            }
        }

        // Insert into the table's B+ tree
        let new_root = self.btree_insert(schema_mut.root_page, pk_key.clone(), value)?;

        // Insert into all indexes
        for index in &schema_mut.indexes {
            let index_key = self.build_index_key(&schema_mut, index, &row)?;
            self.btree_insert(index.root_page, index_key, pk_key.clone())?;
        }

        // Update schema if root changed or auto_increment was used
        if new_root != schema_mut.root_page || schema_modified {
            let schema_entry = self.schemas.get_mut(table_name).unwrap();
            schema_entry.root_page = new_root;
            schema_entry.row_count += 1;
            if schema_modified {
                schema_entry.auto_increment = schema_mut.auto_increment;
            }
            let schema_clone = schema_entry.clone();
            self.save_schema(&schema_clone)?;
        } else {
            let schema_entry = self.schemas.get_mut(table_name).unwrap();
            schema_entry.row_count += 1;
        }

        Ok(())
    }

    /// Get a row by primary key
    pub fn get(&mut self, table_name: &str, pk_values: &[Value]) -> io::Result<Option<Row>> {
        let schema = self.schemas.get(table_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?
            .clone();

        let key = self.serialize_pk(pk_values);

        if let Some(value) = self.btree_search(schema.root_page, &key)? {
            Ok(Some(Row::deserialize(&value, &schema)?))
        } else {
            Ok(None)
        }
    }

    /// Delete a row by primary key
    pub fn delete(&mut self, table_name: &str, pk_values: &[Value]) -> io::Result<bool> {
        let schema = self.schemas.get(table_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?
            .clone();

        let pk_key = self.serialize_pk(pk_values);

        // If there are indexes, we need to look up the row first to get indexed values
        if !schema.indexes.is_empty() {
            if let Some(row_data) = self.btree_search(schema.root_page, &pk_key)? {
                let row = Row::deserialize(&row_data, &schema)?;

                // Delete from each index
                for index in &schema.indexes {
                    let index_key = self.build_index_key(&schema, index, &row)?;
                    self.btree_delete(index.root_page, &index_key)?;
                }
            }
        }

        let deleted = self.btree_delete(schema.root_page, &pk_key)?;

        if deleted {
            let schema = self.schemas.get_mut(table_name).unwrap();
            schema.row_count = schema.row_count.saturating_sub(1);
        }

        Ok(deleted)
    }

    /// Scan all rows in a table
    pub fn scan(&mut self, table_name: &str) -> io::Result<Vec<Row>> {
        let schema = self.schemas.get(table_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?
            .clone();

        let entries = self.scan_btree(schema.root_page)?;
        let mut rows = Vec::with_capacity(entries.len());

        for (_key, value) in entries {
            rows.push(Row::deserialize(&value, &schema)?);
        }

        Ok(rows)
    }

    /// Range scan: get rows where PK is in [start, end)
    pub fn range_scan(
        &mut self,
        table_name: &str,
        start: Option<&[Value]>,
        end: Option<&[Value]>,
    ) -> io::Result<Vec<Row>> {
        let schema = self.schemas.get(table_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?
            .clone();

        let start_key = start.map(|v| self.serialize_pk(v));
        let end_key = end.map(|v| self.serialize_pk(v));

        let entries = self.btree_range_scan(
            schema.root_page,
            start_key.as_deref(),
            end_key.as_deref(),
        )?;

        let mut rows = Vec::with_capacity(entries.len());
        for (_key, value) in entries {
            rows.push(Row::deserialize(&value, &schema)?);
        }

        Ok(rows)
    }

    // -------------------------------------------------------------------------
    // Index Operations
    // -------------------------------------------------------------------------

    /// Create a secondary index on a table
    pub fn create_index(
        &mut self,
        table_name: &str,
        index_name: &str,
        columns: &[&str],
        unique: bool,
    ) -> io::Result<()> {
        // Validate table exists
        let schema = self.schemas.get(table_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?
            .clone();

        // Check index doesn't already exist
        if schema.get_index(index_name).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("index '{}' already exists", index_name),
            ));
        }

        // Validate columns exist
        for col_name in columns {
            if schema.get_column(col_name).is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("column '{}' does not exist", col_name),
                ));
            }
        }

        // Allocate root page for index B+ tree
        let root_page = self.allocate_page()?;

        // Initialize as empty leaf node
        let leaf = LeafNode::new();
        self.write_page(root_page, &leaf.serialize())?;

        // Create index definition
        let mut idx = IndexDef::new(
            index_name,
            columns.iter().map(|s| s.to_string()).collect(),
            unique,
        );
        idx.root_page = root_page;

        // Scan all existing rows and populate the index
        let entries = self.scan_btree(schema.root_page)?;
        for (_pk_key, row_data) in entries {
            let row = Row::deserialize(&row_data, &schema)?;
            let pk_key = self.build_pk_key(&schema, &row)?;
            let index_key = self.build_index_key(&schema, &idx, &row)?;

            // Check unique constraint
            if unique {
                let prefix = self.build_index_key_prefix(&schema, &idx, &row)?;
                let existing = self.btree_range_scan_prefix(idx.root_page, &prefix)?;
                if !existing.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("duplicate key violates unique constraint on index '{}'", index_name),
                    ));
                }
            }

            // Insert into index: key = indexed values + PK, value = PK
            self.btree_insert(idx.root_page, index_key, pk_key)?;
        }

        // Add index to schema
        let schema_entry = self.schemas.get_mut(table_name).unwrap();
        schema_entry.indexes.push(idx);
        let schema_clone = schema_entry.clone();
        self.save_schema(&schema_clone)?;

        Ok(())
    }

    /// Find rows by index (exact match on indexed columns)
    pub fn find_by_index(
        &mut self,
        table_name: &str,
        index_name: &str,
        values: &[Value],
    ) -> io::Result<Vec<Row>> {
        let schema = self.schemas.get(table_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?
            .clone();

        let index = schema.get_index(index_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound,
                format!("index '{}' not found", index_name)))?
            .clone();

        // Build key prefix from values
        let prefix = self.serialize_values(values);

        // Range scan the index for all keys with that prefix
        let index_entries = self.btree_range_scan_prefix(index.root_page, &prefix)?;

        // For each match, look up the actual row by PK
        let mut rows = Vec::with_capacity(index_entries.len());
        for (_index_key, pk_key) in index_entries {
            if let Some(row_data) = self.btree_search(schema.root_page, &pk_key)? {
                rows.push(Row::deserialize(&row_data, &schema)?);
            }
        }

        Ok(rows)
    }

    /// Range scan on an index
    pub fn index_range_scan(
        &mut self,
        table_name: &str,
        index_name: &str,
        start: Option<&[Value]>,
        end: Option<&[Value]>,
    ) -> io::Result<Vec<Row>> {
        let schema = self.schemas.get(table_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?
            .clone();

        let index = schema.get_index(index_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound,
                format!("index '{}' not found", index_name)))?
            .clone();

        let start_key = start.map(|v| self.serialize_values(v));
        let end_key = end.map(|v| self.serialize_values(v));

        // Range scan the index
        let index_entries = self.btree_range_scan(
            index.root_page,
            start_key.as_deref(),
            end_key.as_deref(),
        )?;

        // For each match, look up the actual row by PK
        let mut rows = Vec::with_capacity(index_entries.len());
        for (_index_key, pk_key) in index_entries {
            if let Some(row_data) = self.btree_search(schema.root_page, &pk_key)? {
                rows.push(Row::deserialize(&row_data, &schema)?);
            }
        }

        Ok(rows)
    }

    /// Drop an index
    pub fn drop_index(&mut self, table_name: &str, index_name: &str) -> io::Result<()> {
        let schema = self.schemas.get_mut(table_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?;

        let idx_pos = schema.indexes.iter().position(|idx| idx.name == index_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound,
                format!("index '{}' not found", index_name)))?;

        // Remove from schema (page reclamation deferred)
        schema.indexes.remove(idx_pos);
        let schema_clone = schema.clone();
        self.save_schema(&schema_clone)?;

        Ok(())
    }

    // -------------------------------------------------------------------------
    // Index Helper Methods
    // -------------------------------------------------------------------------

    /// Build index key: indexed column values + PK
    fn build_index_key(&self, schema: &TableSchema, index: &IndexDef, row: &Row) -> io::Result<Vec<u8>> {
        let mut key = Vec::new();

        // Serialize indexed column values
        for col_name in &index.columns {
            let idx = schema.get_column_index(col_name)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "index column not found"))?;
            row.values[idx].serialize(&mut key);
        }

        // Append PK to ensure uniqueness
        let pk_key = self.build_pk_key(schema, row)?;
        key.extend_from_slice(&pk_key);

        Ok(key)
    }

    pub fn index_nested_loop_join(
        &mut self,
        left_table: &str,
        right_table: &str,
        left_col: &str,
        right_col: &str,
        right_index: &str,
        join_type: JoinType,
    ) -> io::Result<Vec<JoinedRow>> {
        // Validate schemas
        let left_schema = self.schemas.get(left_table)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound,
                                          format!("left table '{}' not found", left_table)))?
            .clone();

        let right_schema = self.schemas.get(right_table)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound,
                                          format!("right table '{}' not found", right_table)))?
            .clone();

        // Validate columns exist
        let left_col_idx = left_schema.get_column_index(left_col)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
                                          format!("column '{}' not found in table '{}'", left_col, left_table)))?;

        let _right_col_idx = right_schema.get_column_index(right_col)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
                                          format!("column '{}' not found in table '{}'", right_col, right_table)))?;

        // Validate index exists on right table
        if right_schema.get_index(right_index).is_none() {
            return Err(io::Error::new(io::ErrorKind::NotFound,
                                      format!("index '{}' not found on table '{}'", right_index, right_table)));
        }

        // Scan the left (outer) table
        let left_rows = self.scan(left_table)?;
        let mut results = Vec::new();

        // For each row in the left table
        for left_row in left_rows {
            let join_value = &left_row.values[left_col_idx];

            // Use the index to find matching rows in the right table
            let right_matches = self.find_by_index(
                right_table,
                right_index,
                &[join_value.clone()],
            )?;

            if right_matches.is_empty() {
                // No matches found
                match join_type {
                    JoinType::Inner => {
                        // Skip this left row for inner joins
                        continue;
                    }
                    JoinType::LeftOuter => {
                        // Include left row with NULLs for right side
                        let null_right = Row::new(
                            vec![Value::Null; right_schema.columns.len()]
                        );
                        results.push(JoinedRow::new(left_row.clone(), null_right));
                    }
                }
            } else {
                // Found matches - add all combinations
                for right_row in right_matches {
                    results.push(JoinedRow::new(left_row.clone(), right_row));
                }
            }
        }

        Ok(results)
    }

    /// Perform a simple nested loop join (no index required, but slower)
    /// Useful when no index exists on the join column
    pub fn nested_loop_join(
        &mut self,
        left_table: &str,
        right_table: &str,
        left_col: &str,
        right_col: &str,
        join_type: JoinType,
    ) -> io::Result<Vec<JoinedRow>> {
        let left_schema = self.schemas.get(left_table)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound,
                                          format!("left table '{}' not found", left_table)))?
            .clone();

        let right_schema = self.schemas.get(right_table)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound,
                                          format!("right table '{}' not found", right_table)))?
            .clone();

        let left_col_idx = left_schema.get_column_index(left_col)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
                                          format!("column '{}' not found in table '{}'", left_col, left_table)))?;

        let right_col_idx = right_schema.get_column_index(right_col)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
                                          format!("column '{}' not found in table '{}'", right_col, right_table)))?;

        let left_rows = self.scan(left_table)?;
        let right_rows = self.scan(right_table)?;
        let mut results = Vec::new();

        for left_row in left_rows {
            let left_value = &left_row.values[left_col_idx];
            let mut found_match = false;

            for right_row in &right_rows {
                let right_value = &right_row.values[right_col_idx];

                if left_value == right_value {
                    results.push(JoinedRow::new(left_row.clone(), right_row.clone()));
                    found_match = true;
                }
            }

            // Handle left outer join with no matches
            if !found_match && join_type == JoinType::LeftOuter {
                let null_right = Row::new(
                    vec![Value::Null; right_schema.columns.len()]
                );
                results.push(JoinedRow::new(left_row.clone(), null_right));
            }
        }

        Ok(results)
    }

    /// Build index key prefix (without PK, for lookups)
    fn build_index_key_prefix(&self, schema: &TableSchema, index: &IndexDef, row: &Row) -> io::Result<Vec<u8>> {
        let mut prefix = Vec::new();

        for col_name in &index.columns {
            let idx = schema.get_column_index(col_name)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "index column not found"))?;
            row.values[idx].serialize(&mut prefix);
        }

        Ok(prefix)
    }

    /// Serialize a slice of values
    fn serialize_values(&self, values: &[Value]) -> Vec<u8> {
        let mut buf = Vec::new();
        for v in values {
            v.serialize(&mut buf);
        }
        buf
    }

    /// Range scan with prefix matching (for index lookups)
    fn btree_range_scan_prefix(
        &mut self,
        root: u64,
        prefix: &[u8],
    ) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut results = Vec::new();

        // Find starting leaf
        let (start_leaf, _) = self.find_leaf_with_path(root, prefix)?;
        let mut current = start_leaf;

        'outer: loop {
            let page_data = self.read_page(current)?;
            let leaf = LeafNode::deserialize(&page_data)?;

            for (k, v) in leaf.keys.iter().zip(leaf.values.iter()) {
                // Check if key starts with prefix
                if k.len() >= prefix.len() && &k[..prefix.len()] == prefix {
                    results.push((k.clone(), v.clone()));
                } else if k.as_slice() > prefix && !k.starts_with(prefix) {
                    // Past the prefix range
                    break 'outer;
                }
            }

            if leaf.next_leaf == 0 {
                break;
            }
            current = leaf.next_leaf;
        }

        Ok(results)
    }

    // -------------------------------------------------------------------------
    // Primary Key Helpers
    // -------------------------------------------------------------------------

    fn build_pk_key(&self, schema: &TableSchema, row: &Row) -> io::Result<Vec<u8>> {
        let mut key_values = Vec::new();

        for pk_col in &schema.primary_key {
            let idx = schema.get_column_index(pk_col)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "PK column not found"))?;
            key_values.push(row.values[idx].clone());
        }

        Ok(self.serialize_pk(&key_values))
    }

    fn serialize_pk(&self, values: &[Value]) -> Vec<u8> {
        let mut buf = Vec::new();
        for v in values {
            v.serialize(&mut buf);
        }
        buf
    }

    // -------------------------------------------------------------------------
    // B+ Tree Operations
    // -------------------------------------------------------------------------

    fn btree_search(&mut self, root: u64, key: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let mut current_page = root;

        loop {
            let page_data = self.read_page(current_page)?;

            match page_data[0] {
                NODE_INTERNAL => {
                    let node = InternalNode::deserialize(&page_data)?;

                    // Find child to descend into
                    let mut child_idx = node.keys.len();
                    for (i, k) in node.keys.iter().enumerate() {
                        if key < k.as_slice() {
                            child_idx = i;
                            break;
                        }
                    }

                    current_page = node.children[child_idx];
                }
                NODE_LEAF => {
                    let node = LeafNode::deserialize(&page_data)?;

                    // Binary search for key
                    for (i, k) in node.keys.iter().enumerate() {
                        if k.as_slice() == key {
                            return Ok(Some(node.values[i].clone()));
                        }
                        if k.as_slice() > key {
                            break;
                        }
                    }

                    return Ok(None);
                }
                _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown node type")),
            }
        }
    }

    fn btree_insert(&mut self, root: u64, key: Vec<u8>, value: Vec<u8>) -> io::Result<u64> {
        // First, find the leaf node where this key belongs
        let (leaf_page, path) = self.find_leaf_with_path(root, &key)?;

        let page_data = self.read_page(leaf_page)?;
        let mut leaf = LeafNode::deserialize(&page_data)?;

        // Find insertion position
        let pos = leaf.find_key_position(&key);

        // Check if key already exists (update)
        if pos < leaf.keys.len() && leaf.keys[pos] == key {
            leaf.values[pos] = value;
            self.write_page(leaf_page, &leaf.serialize())?;
            return Ok(root);
        }

        // Insert
        leaf.keys.insert(pos, key.clone());
        leaf.values.insert(pos, value);

        if !leaf.is_full() {
            // Simple case: leaf has room
            self.write_page(leaf_page, &leaf.serialize())?;
            Ok(root)
        } else {
            // Need to split
            self.split_and_propagate(leaf_page, leaf, path, root)
        }
    }

    fn find_leaf_with_path(&mut self, root: u64, key: &[u8]) -> io::Result<(u64, Vec<(u64, usize)>)> {
        let mut path = Vec::new();
        let mut current_page = root;

        loop {
            let page_data = self.read_page(current_page)?;

            match page_data[0] {
                NODE_INTERNAL => {
                    let node = InternalNode::deserialize(&page_data)?;

                    let mut child_idx = node.keys.len();
                    for (i, k) in node.keys.iter().enumerate() {
                        if key < k.as_slice() {
                            child_idx = i;
                            break;
                        }
                    }

                    path.push((current_page, child_idx));
                    current_page = node.children[child_idx];
                }
                NODE_LEAF => {
                    return Ok((current_page, path));
                }
                _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown node type")),
            }
        }
    }

    fn split_and_propagate(
        &mut self,
        leaf_page: u64,
        leaf: LeafNode,
        path: Vec<(u64, usize)>,
        root: u64,
    ) -> io::Result<u64> {
        // Split the leaf
        let mid = leaf.keys.len() / 2;

        let mut left = LeafNode::new();
        let mut right = LeafNode::new();

        left.keys = leaf.keys[..mid].to_vec();
        left.values = leaf.values[..mid].to_vec();

        right.keys = leaf.keys[mid..].to_vec();
        right.values = leaf.values[mid..].to_vec();

        // Allocate new page for right sibling
        let right_page = self.allocate_page()?;

        // Update sibling pointers
        left.next_leaf = right_page;
        left.prev_leaf = leaf.prev_leaf;
        right.prev_leaf = leaf_page;
        right.next_leaf = leaf.next_leaf;

        self.write_page(leaf_page, &left.serialize())?;
        self.write_page(right_page, &right.serialize())?;

        // Update next leaf's prev pointer if exists
        if leaf.next_leaf != 0 {
            let mut next_data = self.read_page(leaf.next_leaf)?;
            // Update prev_leaf field (bytes 9-16)
            next_data[9..17].copy_from_slice(&right_page.to_le_bytes());
            self.write_page(leaf.next_leaf, &next_data)?;
        }

        // Promote the first key of the right node
        let promoted_key = right.keys[0].clone();

        self.propagate_split(path, promoted_key, leaf_page, right_page, root)
    }

    fn propagate_split(
        &mut self,
        mut path: Vec<(u64, usize)>,
        key: Vec<u8>,
        left_child: u64,
        right_child: u64,
        root: u64,
    ) -> io::Result<u64> {
        if path.is_empty() {
            // Root was a leaf, create new root
            let new_root_page = self.allocate_page()?;
            let mut new_root = InternalNode::new();
            new_root.keys.push(key);
            new_root.children.push(left_child);
            new_root.children.push(right_child);
            self.write_page(new_root_page, &new_root.serialize())?;
            return Ok(new_root_page);
        }

        let (parent_page, child_idx) = path.pop().unwrap();
        let page_data = self.read_page(parent_page)?;
        let mut parent = InternalNode::deserialize(&page_data)?;

        // Insert the new key and child
        parent.keys.insert(child_idx, key.clone());
        parent.children[child_idx] = left_child;
        parent.children.insert(child_idx + 1, right_child);

        if !parent.is_full() {
            self.write_page(parent_page, &parent.serialize())?;
            Ok(root)
        } else {
            // Split the internal node
            let mid = parent.keys.len() / 2;
            let promoted_key = parent.keys[mid].clone();

            let mut left_internal = InternalNode::new();
            let mut right_internal = InternalNode::new();

            left_internal.keys = parent.keys[..mid].to_vec();
            left_internal.children = parent.children[..=mid].to_vec();

            right_internal.keys = parent.keys[mid + 1..].to_vec();
            right_internal.children = parent.children[mid + 1..].to_vec();

            let right_internal_page = self.allocate_page()?;

            self.write_page(parent_page, &left_internal.serialize())?;
            self.write_page(right_internal_page, &right_internal.serialize())?;

            self.propagate_split(path, promoted_key, parent_page, right_internal_page, root)
        }
    }

    fn btree_delete(&mut self, root: u64, key: &[u8]) -> io::Result<bool> {
        let page_data = self.read_page(root)?;

        match page_data[0] {
            NODE_LEAF => {
                let mut leaf = LeafNode::deserialize(&page_data)?;

                if let Some(pos) = leaf.keys.iter().position(|k| k.as_slice() == key) {
                    leaf.keys.remove(pos);
                    leaf.values.remove(pos);
                    self.write_page(root, &leaf.serialize())?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            NODE_INTERNAL => {
                // For now, just find and delete from leaf
                // TODO: Implement proper rebalancing
                let (leaf_page, _path) = self.find_leaf_with_path(root, key)?;
                let leaf_data = self.read_page(leaf_page)?;
                let mut leaf = LeafNode::deserialize(&leaf_data)?;

                if let Some(pos) = leaf.keys.iter().position(|k| k.as_slice() == key) {
                    leaf.keys.remove(pos);
                    leaf.values.remove(pos);
                    self.write_page(leaf_page, &leaf.serialize())?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, "unknown node type")),
        }
    }

    fn scan_btree(&mut self, root: u64) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut results = Vec::new();

        // Find leftmost leaf
        let mut current = root;
        loop {
            let page_data = self.read_page(current)?;
            match page_data[0] {
                NODE_INTERNAL => {
                    let node = InternalNode::deserialize(&page_data)?;
                    current = node.children[0];
                }
                NODE_LEAF => break,
                _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown node type")),
            }
        }

        // Follow leaf chain
        loop {
            let page_data = self.read_page(current)?;
            let leaf = LeafNode::deserialize(&page_data)?;

            for (k, v) in leaf.keys.iter().zip(leaf.values.iter()) {
                results.push((k.clone(), v.clone()));
            }

            if leaf.next_leaf == 0 {
                break;
            }
            current = leaf.next_leaf;
        }

        Ok(results)
    }

    fn btree_range_scan(
        &mut self,
        root: u64,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut results = Vec::new();

        // Find starting leaf
        let start_leaf = if let Some(start_key) = start {
            let (leaf, _) = self.find_leaf_with_path(root, start_key)?;
            leaf
        } else {
            // Find leftmost leaf
            let mut current = root;
            loop {
                let page_data = self.read_page(current)?;
                match page_data[0] {
                    NODE_INTERNAL => {
                        let node = InternalNode::deserialize(&page_data)?;
                        current = node.children[0];
                    }
                    NODE_LEAF => break current,
                    _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown node type")),
                }
            }
        };

        let mut current = start_leaf;

        'outer: loop {
            let page_data = self.read_page(current)?;
            let leaf = LeafNode::deserialize(&page_data)?;

            for (k, v) in leaf.keys.iter().zip(leaf.values.iter()) {
                // Check start bound
                if let Some(s) = start {
                    if k.as_slice() < s {
                        continue;
                    }
                }

                // Check end bound
                if let Some(e) = end {
                    if k.as_slice() >= e {
                        break 'outer;
                    }
                }

                results.push((k.clone(), v.clone()));
            }

            if leaf.next_leaf == 0 {
                break;
            }
            current = leaf.next_leaf;
        }

        Ok(results)
    }

    /// Flush all pending writes to disk
    pub fn sync(&mut self) -> io::Result<()> {
        self.flush_cache()?;
        self.page_manager.sync()
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // Best effort flush on drop
        let _ = self.sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Column;
    use std::fs;

    fn test_db_path(name: &str) -> String {
        format!("/tmp/test_index_{}.db", name)
    }

    fn cleanup(path: &str) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_create_index_and_find() {
        let path = test_db_path("create_find");
        cleanup(&path);

        {
            let mut db = Database::create(&path).unwrap();

            // Create a table
            let schema = TableSchema::new("users")
                .column(Column::new("id", DataType::Int32))
                .column(Column::new("name", DataType::Text))
                .column(Column::new("email", DataType::Text))
                .primary_key(&["id"]);
            db.create_table(schema).unwrap();

            // Insert some rows
            db.insert("users", Row::new(vec![
                Value::Int32(1),
                Value::Text("Alice".into()),
                Value::Text("alice@example.com".into()),
            ])).unwrap();
            db.insert("users", Row::new(vec![
                Value::Int32(2),
                Value::Text("Bob".into()),
                Value::Text("bob@example.com".into()),
            ])).unwrap();
            db.insert("users", Row::new(vec![
                Value::Int32(3),
                Value::Text("Alice".into()),
                Value::Text("alice2@example.com".into()),
            ])).unwrap();

            // Create an index on name column
            db.create_index("users", "idx_name", &["name"], false).unwrap();

            // Find by index
            let rows = db.find_by_index("users", "idx_name", &[Value::Text("Alice".into())]).unwrap();
            assert_eq!(rows.len(), 2);

            let rows = db.find_by_index("users", "idx_name", &[Value::Text("Bob".into())]).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int32(2));

            let rows = db.find_by_index("users", "idx_name", &[Value::Text("Charlie".into())]).unwrap();
            assert_eq!(rows.len(), 0);
        }

        cleanup(&path);
    }

    #[test]
    fn test_unique_index_constraint() {
        let path = test_db_path("unique");
        cleanup(&path);

        {
            let mut db = Database::create(&path).unwrap();

            let schema = TableSchema::new("users")
                .column(Column::new("id", DataType::Int32))
                .column(Column::new("email", DataType::Text))
                .primary_key(&["id"]);
            db.create_table(schema).unwrap();

            db.insert("users", Row::new(vec![
                Value::Int32(1),
                Value::Text("alice@example.com".into()),
            ])).unwrap();

            // Create a unique index on email
            db.create_index("users", "idx_email", &["email"], true).unwrap();

            // Try to insert a duplicate email - should fail
            let result = db.insert("users", Row::new(vec![
                Value::Int32(2),
                Value::Text("alice@example.com".into()),
            ]));
            assert!(result.is_err());

            // Insert with different email - should succeed
            db.insert("users", Row::new(vec![
                Value::Int32(2),
                Value::Text("bob@example.com".into()),
            ])).unwrap();
        }

        cleanup(&path);
    }

    #[test]
    fn test_index_maintenance_on_insert() {
        let path = test_db_path("insert_maintenance");
        cleanup(&path);

        {
            let mut db = Database::create(&path).unwrap();

            let schema = TableSchema::new("users")
                .column(Column::new("id", DataType::Int32))
                .column(Column::new("name", DataType::Text))
                .primary_key(&["id"]);
            db.create_table(schema).unwrap();

            // Create index first
            db.create_index("users", "idx_name", &["name"], false).unwrap();

            // Insert rows AFTER index creation
            db.insert("users", Row::new(vec![
                Value::Int32(1),
                Value::Text("Alice".into()),
            ])).unwrap();
            db.insert("users", Row::new(vec![
                Value::Int32(2),
                Value::Text("Bob".into()),
            ])).unwrap();

            // Verify index works for newly inserted rows
            let rows = db.find_by_index("users", "idx_name", &[Value::Text("Alice".into())]).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int32(1));
        }

        cleanup(&path);
    }

    #[test]
    fn test_index_maintenance_on_delete() {
        let path = test_db_path("delete_maintenance");
        cleanup(&path);

        {
            let mut db = Database::create(&path).unwrap();

            let schema = TableSchema::new("users")
                .column(Column::new("id", DataType::Int32))
                .column(Column::new("name", DataType::Text))
                .primary_key(&["id"]);
            db.create_table(schema).unwrap();

            db.insert("users", Row::new(vec![
                Value::Int32(1),
                Value::Text("Alice".into()),
            ])).unwrap();
            db.insert("users", Row::new(vec![
                Value::Int32(2),
                Value::Text("Alice".into()),
            ])).unwrap();

            db.create_index("users", "idx_name", &["name"], false).unwrap();

            // Verify both Alice entries exist
            let rows = db.find_by_index("users", "idx_name", &[Value::Text("Alice".into())]).unwrap();
            assert_eq!(rows.len(), 2);

            // Delete one
            db.delete("users", &[Value::Int32(1)]).unwrap();

            // Verify only one remains
            let rows = db.find_by_index("users", "idx_name", &[Value::Text("Alice".into())]).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::Int32(2));
        }

        cleanup(&path);
    }

    #[test]
    fn test_drop_index() {
        let path = test_db_path("drop");
        cleanup(&path);

        {
            let mut db = Database::create(&path).unwrap();

            let schema = TableSchema::new("users")
                .column(Column::new("id", DataType::Int32))
                .column(Column::new("name", DataType::Text))
                .primary_key(&["id"]);
            db.create_table(schema).unwrap();

            db.create_index("users", "idx_name", &["name"], false).unwrap();

            // Verify index exists
            assert!(db.get_schema("users").unwrap().get_index("idx_name").is_some());

            // Drop index
            db.drop_index("users", "idx_name").unwrap();

            // Verify index no longer exists
            assert!(db.get_schema("users").unwrap().get_index("idx_name").is_none());
        }

        cleanup(&path);
    }

    #[test]
    fn test_index_range_scan() {
        let path = test_db_path("range");
        cleanup(&path);

        {
            let mut db = Database::create(&path).unwrap();

            let schema = TableSchema::new("users")
                .column(Column::new("id", DataType::Int32))
                .column(Column::new("score", DataType::Int32))
                .primary_key(&["id"]);
            db.create_table(schema).unwrap();

            // Insert rows
            for i in 0..10 {
                db.insert("users", Row::new(vec![
                    Value::Int32(i),
                    Value::Int32(i * 10),
                ])).unwrap();
            }

            // Create index on score
            db.create_index("users", "idx_score", &["score"], false).unwrap();

            // Range scan: scores from 30 to 60 (exclusive)
            let rows = db.index_range_scan(
                "users",
                "idx_score",
                Some(&[Value::Int32(30)]),
                Some(&[Value::Int32(60)]),
            ).unwrap();

            assert_eq!(rows.len(), 3); // scores 30, 40, 50
        }

        cleanup(&path);
    }
}
