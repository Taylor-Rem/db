use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

use crate::btree::{InternalNode, LeafNode, NODE_INTERNAL, NODE_LEAF};
use crate::row::Row;
use crate::schema::{Column, TableSchema};
use crate::storage::{FileHeader, PageCache, PageManager, Page, PAGE_SIZE};
use crate::value::Value;

/// Default page cache size (number of pages)
const DEFAULT_CACHE_SIZE: usize = 256;

pub struct Database {
    page_manager: PageManager,
    cache: PageCache,
    header: FileHeader,
    schemas: HashMap<String, TableSchema>,
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

        // The schema catalog is itself a B+ tree where:
        // - Key: table name (as bytes)
        // - Value: serialized TableSchema

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
    pub fn insert(&mut self, table_name: &str, row: Row) -> io::Result<()> {
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

        // Build the primary key
        let key = self.build_pk_key(&schema, &row)?;
        let value = row.serialize(&schema);

        // Insert into the table's B+ tree
        let new_root = self.btree_insert(schema.root_page, key, value)?;

        // Update schema if root changed
        if new_root != schema.root_page {
            let schema = self.schemas.get_mut(table_name).unwrap();
            schema.root_page = new_root;
            schema.row_count += 1;
            let schema_clone = schema.clone();
            self.save_schema(&schema_clone)?;
        } else {
            let schema = self.schemas.get_mut(table_name).unwrap();
            schema.row_count += 1;
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

        let key = self.serialize_pk(pk_values);
        let deleted = self.btree_delete(schema.root_page, &key)?;

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
