# db — A B+ Tree Embedded Database Engine (Rust)

A small, single-file embedded database engine written in Rust. It stores tables in
on-disk B+ trees with a paged storage layer, an LRU page cache, secondary indexes,
basic joins, and a fluent query builder. Think of it as a learning-scale SQLite-style
storage engine (no SQL parser — you drive it through a Rust API).

- **Edition:** Rust 2024
- **Dependencies:** `memmap2`, `bincode`, `serde` (note: currently the code uses manual
  little-endian byte serialization throughout; `bincode`/`serde` are declared but not
  yet doing much real work)

---

## Architecture overview

Data flows top to bottom: a caller uses the `Database` API (or the `QueryBuilder`),
which manipulates rows, which are turned into B+ tree nodes, which are read/written as
fixed-size pages through a cache and a page manager.

```
QueryBuilder  ──►  Database  ──►  B+ tree (Internal/Leaf nodes)
                      │                    │
                   Schemas             Pages (4096 bytes)
                      │                    │
                      └────►  PageCache (LRU)  ──►  PageManager (file I/O)
                                                          │
                                                    on-disk file
```

### Module map

| Module | Responsibility |
| --- | --- |
| `data_type.rs` | `DataType` enum (Null, Bool, UInt32/64, Int32/64, Float64, Text, Blob, Timestamp) and fixed-size lookup. |
| `value.rs` | `Value` enum holding actual cell data, plus manual `serialize`/`deserialize` and key comparison. |
| `row.rs` | `Row` (a `Vec<Value>`); serializes with a leading null bitmap, then non-null values. |
| `schema/` | `Column`, `IndexDef`, `TableSchema`, and the `TableBuilder` fluent builder. |
| `storage/file_header.rs` | `FileHeader` — magic `"BPDB"`, version, page size, total pages, free-list head, schema-catalog root. Occupies page 0. |
| `storage/page.rs` | `Page` (4096-byte buffer + dirty flag), `PageManager` (seek/read/write/sync), `PageCache` (LRU with eviction). |
| `btree/internal_node.rs` | `InternalNode` (keys + child page numbers); order 128. |
| `btree/leaf_node.rs` | `LeafNode` (keys + values + next/prev leaf pointers); splits at 30 entries. |
| `database.rs` | The engine: table/schema management, row CRUD, indexes, joins, and all B+ tree operations. |
| `query_builder.rs` | Fluent `QueryBuilder`: `from`/`select`/`join`/`where_eq`/`order_by`/`limit` plus `insert`/`update`/`delete`/`execute`. |
| `lib.rs` | Module wiring and convenience re-exports. |

### On-disk format

- The file is a sequence of fixed **4096-byte pages**. Page 0 is the file header.
- Each table and each index is its own **B+ tree**, identified by its root page number.
- Table schemas are themselves stored in a **schema catalog B+ tree**, whose root is
  recorded in the file header (`schema_catalog_root`). On `open()`, the catalog is
  scanned to repopulate the in-memory schema map.
- Node pages begin with a 1-byte tag: `0x01` internal, `0x02` leaf (see `NODE_INTERNAL`
  / `NODE_LEAF`). Leaf nodes also store sibling pointers for ordered scans.

### Keys

- Primary keys are built by serializing the PK column values in order (`build_pk_key` /
  `serialize_pk`).
- Index keys are the indexed column values **followed by the primary key**, which keeps
  duplicate indexed values distinct and lets a lookup recover the row's PK. The index's
  stored value is the PK itself, used to fetch the full row from the table tree.

---

## Feature summary

- **Tables** with typed columns, nullable flags, defaults, and auto-increment columns.
- **Primary keys** (single or composite) backed by a B+ tree.
- **Secondary indexes**, unique and non-unique, maintained automatically on insert/delete.
- **Row operations:** `insert`, `get`, `delete`, `scan`, `range_scan`.
- **Index operations:** `create_index`, `find_by_index`, `index_range_scan`, `drop_index`.
- **Joins:** `index_nested_loop_join` (uses an index on the right table) and
  `nested_loop_join` (no index); both support `Inner` and `LeftOuter`.
- **Query builder** with WHERE (auto-uses an index for equality when one exists),
  ORDER BY, LIMIT, column projection, single-join support, and insert/update/delete.
- **Paging + LRU cache** with dirty-page tracking and flush-on-sync/drop.

---

## Quick start

```rust
use db::{Database, TableSchema, Column, DataType, Value, Row};

let mut db = Database::create("my.db")?;

let schema = TableSchema::new("users")
    .column(Column::new("id", DataType::Int32))
    .column(Column::new("name", DataType::Text))
    .column(Column::new("email", DataType::Text))
    .primary_key(&["id"]);
db.create_table(schema)?;

db.insert("users", Row::new(vec![
    Value::Int32(1),
    Value::Text("Alice".into()),
    Value::Text("alice@example.com".into()),
]))?;

// Secondary index + lookup
db.create_index("users", "idx_name", &["name"], false)?;
let alices = db.find_by_index("users", "idx_name", &[Value::Text("Alice".into())])?;

db.sync()?; // flush to disk (also done best-effort on Drop)
```

Query builder:

```rust
use db::query_builder::{QueryBuilder, OrderDirection};

let result = QueryBuilder::new(&mut db)
    .from("users")
    .where_eq("name", Value::Text("Alice".into()))
    .order_by("id", OrderDirection::Asc)
    .limit(10)
    .execute()?;
```

---

## Build & test

```bash
cargo build
cargo test        # tests live in database.rs and storage/page.rs; write to /tmp/*.db
```

---

## Known limitations & TODOs

These are worth knowing before extending the engine:

1. **UInt32 / UInt64 serialization is broken.** In `value.rs`, both serialize via
   `buf.push(*u as u8)` (one byte), but `deserialize` reads 4 and 8 bytes respectively.
   Round-tripping those types will corrupt data or read past the value. This looks like
   a genuine bug rather than an intentional limitation — likely the first thing to fix.
2. **B+ tree delete does not rebalance.** `btree_delete` removes the entry from the leaf
   but never merges/redistributes underfull nodes (marked TODO), and it doesn't fix up
   separator keys held in internal nodes. Trees only grow structurally; they don't shrink.
3. **No page reclamation.** The header has a `free_list_head`, but `drop_table` and
   `drop_index` only remove catalog/schema entries — pages are never freed or reused
   (`allocate_page` always appends). The `PageType::Overflow` variant and free-list logic
   are unimplemented.
4. **Leaf "fullness" is a fixed entry count (30),** not a byte-size check, so large Text/
   Blob values could in principle overflow a 4096-byte page during `serialize`. There is
   no overflow-page handling for oversized values.
5. **Linear scans where comments claim binary search.** `LeafNode::find_key_position`
   and the leaf scan in `btree_search` walk keys linearly.
6. **Single join only** in the query builder (`execute_join` errors on more than one),
   and WHERE supports a single condition.
7. **serde/bincode are dependencies but largely unused** — serialization is hand-rolled.

---

## Suggested next steps

- Fix the UInt32/UInt64 serialize/deserialize mismatch and add a round-trip test per type.
- Implement free-list-based page reuse and wire up `drop_table`/`drop_index` to reclaim pages.
- Add B+ tree node merging/redistribution on delete.
- Add overflow pages (the type already exists) or a real byte-budget check for leaf fullness.
- Broaden WHERE (multiple predicates, ranges) and multi-join support in the query builder.
