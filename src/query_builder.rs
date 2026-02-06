use std::io;
use crate::{Database, Row, Value, JoinedRow, JoinType};

/// A fluent query builder for database operations
pub struct QueryBuilder<'a> {
    db: &'a mut Database,
    table: Option<String>,
    columns: Vec<String>,
    joins: Vec<JoinClause>,
    where_clause: Option<WhereClause>,
    order_by: Option<(String, OrderDirection)>,
    limit: Option<usize>,
    values: Option<Vec<Value>>,
    set_clauses: Vec<(String, Value)>,
}

struct JoinClause {
    join_type: JoinType,
    table: String,
    left_col: String,
    right_col: String,
    index: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WhereClause {
    column: String,
    op: CompareOp,
    value: Value,
}

#[derive(Debug, Clone, Copy)]
pub enum CompareOp {
    Eq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
}

#[derive(Debug, Clone, Copy)]
pub enum OrderDirection {
    Asc,
    Desc,
}

/// Result of a query that may involve joins
#[derive(Debug)]
pub enum QueryResult {
    /// Simple table scan result
    Simple(Vec<Row>),
    /// Join result with multiple tables
    Joined(Vec<JoinedRow>),
}

impl<'a> QueryBuilder<'a> {
    pub fn new(db: &'a mut Database) -> Self {
        Self {
            db,
            table: None,
            columns: Vec::new(),
            joins: Vec::new(),
            where_clause: None,
            order_by: None,
            limit: None,
            values: None,
            set_clauses: Vec::new(),
        }
    }

    /// Specify the table to query from
    pub fn from(mut self, table: impl Into<String>) -> Self {
        self.table = Some(table.into());
        self
    }

    /// Select specific columns (default: all columns)
    pub fn select(mut self, columns: &[&str]) -> Self {
        self.columns = columns.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Add an inner join
    pub fn inner_join(
        mut self,
        table: impl Into<String>,
        left_col: impl Into<String>,
        right_col: impl Into<String>,
    ) -> Self {
        self.joins.push(JoinClause {
            join_type: JoinType::Inner,
            table: table.into(),
            left_col: left_col.into(),
            right_col: right_col.into(),
            index: None,
        });
        self
    }

    /// Add an inner join with explicit index hint
    pub fn inner_join_indexed(
        mut self,
        table: impl Into<String>,
        left_col: impl Into<String>,
        right_col: impl Into<String>,
        index: impl Into<String>,
    ) -> Self {
        self.joins.push(JoinClause {
            join_type: JoinType::Inner,
            table: table.into(),
            left_col: left_col.into(),
            right_col: right_col.into(),
            index: Some(index.into()),
        });
        self
    }

    /// Add a left outer join
    pub fn left_join(
        mut self,
        table: impl Into<String>,
        left_col: impl Into<String>,
        right_col: impl Into<String>,
    ) -> Self {
        self.joins.push(JoinClause {
            join_type: JoinType::LeftOuter,
            table: table.into(),
            left_col: left_col.into(),
            right_col: right_col.into(),
            index: None,
        });
        self
    }

    /// Add a WHERE clause (currently supports single condition)
    pub fn where_eq(mut self, column: impl Into<String>, value: Value) -> Self {
        self.where_clause = Some(WhereClause {
            column: column.into(),
            op: CompareOp::Eq,
            value,
        });
        self
    }

    /// Add ORDER BY clause
    pub fn order_by(mut self, column: impl Into<String>, direction: OrderDirection) -> Self {
        self.order_by = Some((column.into(), direction));
        self
    }

    /// Limit the number of results
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Set row values for an insert operation
    pub fn values(mut self, values: Vec<Value>) -> Self {
        self.values = Some(values);
        self
    }

    /// Add a column/value pair for an update operation
    pub fn set(mut self, column: impl Into<String>, value: Value) -> Self {
        self.set_clauses.push((column.into(), value));
        self
    }

    /// Insert a row into the table
    pub fn insert(mut self) -> io::Result<()> {
        let table = self.table.take().ok_or_else(||
            io::Error::new(io::ErrorKind::InvalidInput, "no table specified")
        )?;
        let values = self.values.take().ok_or_else(||
            io::Error::new(io::ErrorKind::InvalidInput, "no values specified")
        )?;
        self.db.insert(&table, Row::new(values))
    }

    /// Update rows matching the WHERE clause
    pub fn update(mut self) -> io::Result<usize> {
        let table = self.table.take().ok_or_else(||
            io::Error::new(io::ErrorKind::InvalidInput, "no table specified")
        )?;
        let where_clause = self.where_clause.take();
        let set_clauses = std::mem::take(&mut self.set_clauses);

        if set_clauses.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "no SET clauses specified"));
        }

        let rows = if let Some(ref wc) = where_clause {
            self.apply_where(&table, wc)?
        } else {
            self.db.scan(&table)?
        };

        let mut count = 0;
        for row in &rows {
            let (pk_indices, set_indices) = {
                let schema = self.db.get_schema(&table)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?;

                let pk_indices: Vec<usize> = schema.primary_key.iter()
                    .map(|pk_col| schema.get_column_index(pk_col)
                        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "PK column not found")))
                    .collect::<io::Result<Vec<_>>>()?;

                let set_indices: Vec<(usize, Value)> = set_clauses.iter()
                    .map(|(col, val)| {
                        let idx = schema.get_column_index(col)
                            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
                                format!("column '{}' not found", col)))?;
                        Ok((idx, val.clone()))
                    })
                    .collect::<io::Result<Vec<_>>>()?;

                (pk_indices, set_indices)
            };

            let pk_values: Vec<Value> = pk_indices.iter()
                .map(|&idx| row.values[idx].clone())
                .collect();

            let mut new_values = row.values.clone();
            for (idx, val) in &set_indices {
                new_values[*idx] = val.clone();
            }

            self.db.delete(&table, &pk_values)?;
            self.db.insert(&table, Row::new(new_values))?;
            count += 1;
        }

        Ok(count)
    }

    /// Delete rows matching the WHERE clause
    pub fn delete(mut self) -> io::Result<usize> {
        let table = self.table.take().ok_or_else(||
            io::Error::new(io::ErrorKind::InvalidInput, "no table specified")
        )?;
        let where_clause = self.where_clause.take();

        let rows = if let Some(ref wc) = where_clause {
            self.apply_where(&table, wc)?
        } else {
            self.db.scan(&table)?
        };

        let mut count = 0;
        for row in &rows {
            let pk_values = {
                let schema = self.db.get_schema(&table)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?;

                schema.primary_key.iter()
                    .map(|pk_col| {
                        let idx = schema.get_column_index(pk_col)
                            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "PK column not found"))?;
                        Ok(row.values[idx].clone())
                    })
                    .collect::<io::Result<Vec<Value>>>()?
            };

            if self.db.delete(&table, &pk_values)? {
                count += 1;
            }
        }

        Ok(count)
    }

    /// Execute the query
    pub fn execute(mut self) -> io::Result<QueryResult> {
        let table = self.table.take().ok_or_else(||
            io::Error::new(io::ErrorKind::InvalidInput, "no table specified")
        )?;

        if self.joins.is_empty() {
            // Simple single-table query
            self.execute_simple(&table)
        } else {
            // Join query
            self.execute_join(&table)
        }
    }

    fn execute_simple(mut self, table: &str) -> io::Result<QueryResult> {
        // Get rows based on WHERE clause
        let where_clause = self.where_clause.take();
        let mut rows = if let Some(ref wc) = where_clause {
            self.apply_where(table, wc)?
        } else {
            self.db.scan(table)?
        };

        // Apply ORDER BY
        if let Some((col, direction)) = &self.order_by {
            let schema = self.db.get_schema(table)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?;
            let col_idx = schema.get_column_index(col)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "column not found"))?;

            rows.sort_by(|a, b| {
                let cmp = compare_values(&a.values[col_idx], &b.values[col_idx]);
                match direction {
                    OrderDirection::Asc => cmp,
                    OrderDirection::Desc => cmp.reverse(),
                }
            });
        }

        // Apply LIMIT
        if let Some(n) = self.limit {
            rows.truncate(n);
        }

        // Apply column selection
        if !self.columns.is_empty() {
            rows = self.project_columns(table, rows)?;
        }

        Ok(QueryResult::Simple(rows))
    }

    fn execute_join(mut self, left_table: &str) -> io::Result<QueryResult> {
        if self.joins.len() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "currently only single joins are supported"
            ));
        }

        let join = self.joins.remove(0);

        // Try to find an appropriate index automatically if not specified
        let index_name = if let Some(ref idx) = join.index {
            idx.clone()
        } else {
            // Try to find an index on the right column
            let right_schema = self.db.get_schema(&join.table)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?;

            right_schema.indexes.iter()
                .find(|idx| idx.columns.len() == 1 && idx.columns[0] == join.right_col)
                .map(|idx| idx.name.clone())
                .ok_or_else(|| io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("no index found on column '{}'", join.right_col)
                ))?
        };

        // Perform the join
        let mut results = self.db.index_nested_loop_join(
            left_table,
            &join.table,
            &join.left_col,
            &join.right_col,
            &index_name,
            join.join_type,
        )?;

        // Apply LIMIT
        if let Some(n) = self.limit {
            results.truncate(n);
        }

        Ok(QueryResult::Joined(results))
    }

    fn apply_where(&mut self, table: &str, where_clause: &WhereClause) -> io::Result<Vec<Row>> {
        let (col_idx, index_name) = {
            let schema = self.db.get_schema(table)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?;

            let col_idx = schema.get_column_index(&where_clause.column)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "column not found"))?;

            let index_name = if matches!(where_clause.op, CompareOp::Eq) {
                schema.indexes.iter()
                    .find(|idx| idx.columns.len() == 1 && idx.columns[0] == where_clause.column)
                    .map(|idx| idx.name.clone())
            } else {
                None
            };

            (col_idx, index_name)
        };

        // Try to use an index if available
        if let Some(index_name) = index_name {
            return self.db.find_by_index(table, &index_name, &[where_clause.value.clone()]);
        }

        // Fall back to full table scan with filter
        let all_rows = self.db.scan(table)?;
        Ok(all_rows.into_iter()
            .filter(|row| {
                let val = &row.values[col_idx];
                match where_clause.op {
                    CompareOp::Eq => val == &where_clause.value,
                    CompareOp::NotEq => val != &where_clause.value,
                    CompareOp::Lt => compare_values(val, &where_clause.value).is_lt(),
                    CompareOp::Lte => !compare_values(val, &where_clause.value).is_gt(),
                    CompareOp::Gt => compare_values(val, &where_clause.value).is_gt(),
                    CompareOp::Gte => !compare_values(val, &where_clause.value).is_lt(),
                }
            })
            .collect())
    }

    fn project_columns(&self, table: &str, rows: Vec<Row>) -> io::Result<Vec<Row>> {
        let schema = self.db.get_schema(table)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "table not found"))?;

        let col_indices: Vec<usize> = self.columns.iter()
            .map(|col| schema.get_column_index(col)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
                                              format!("column '{}' not found", col))))
            .collect::<io::Result<Vec<_>>>()?;

        Ok(rows.into_iter()
            .map(|row| {
                let values = col_indices.iter()
                    .map(|&idx| row.values[idx].clone())
                    .collect();
                Row::new(values)
            })
            .collect())
    }
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        (Value::Int32(x), Value::Int32(y)) => x.cmp(y),
        (Value::Int64(x), Value::Int64(y)) => x.cmp(y),
        (Value::UInt32(x), Value::UInt32(y)) => x.cmp(y),
        (Value::UInt64(x), Value::UInt64(y)) => x.cmp(y),
        (Value::Float64(x), Value::Float64(y)) => {
            x.partial_cmp(y).unwrap_or(Ordering::Equal)
        }
        (Value::Text(x), Value::Text(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Timestamp(x), Value::Timestamp(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}