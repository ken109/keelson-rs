use std::collections::HashMap;
use std::sync::Arc;

use keelson_core::{FromValue, Value};

use crate::error::ExecError;

/// One column of a result set's header.
#[derive(Debug, Clone)]
pub struct Column {
    name: String,
}

impl Column {
    /// A column with this name. Backend-facing.
    pub fn new(name: impl Into<String>) -> Self {
        Column { name: name.into() }
    }

    /// The column's name, exactly as the driver reported it.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Headers at or below this width are searched by scanning.
///
/// Measured on this crate's own rows: reading every column of 5,000 rows by
/// name costs 1.29x a scan against a prepared hash map at 8 columns, 2.2x at
/// 30 and 7x at 80 — but at 8 columns the whole name lookup is only 5% of
/// decoding, so the map would buy about 1% for an allocation every result
/// set makes. By 30 columns the lookup is 23% of decoding and by 80 it is
/// 66%, which is what the map is for.
const SCAN_LIMIT: usize = 8;

/// A result set's column header, with the name lookup prepared once.
///
/// Rows of one result set share this. That sharing is the point: resolving a
/// column name used to be a scan of the header *per read*, so a wide row cost
/// O(columns) name comparisons for each of its own columns — quadratic in the
/// width, and by 80 columns two thirds of the time spent decoding. A backend
/// builds one of these when the first row arrives and clones the `Arc` for
/// every row after it.
#[derive(Debug, Clone)]
pub struct Header {
    columns: Arc<[Column]>,
    /// `None` for a header narrow enough that scanning wins — see
    /// [`SCAN_LIMIT`].
    index: Option<HashMap<Box<str>, usize>>,
}

impl Header {
    /// Prepare a header for a result set. Backend-facing.
    pub fn new(columns: impl Into<Arc<[Column]>>) -> Self {
        let columns = columns.into();
        let index = (columns.len() > SCAN_LIMIT).then(|| {
            let mut m = HashMap::with_capacity(columns.len());
            for (i, c) in columns.iter().enumerate() {
                // First wins, matching the scan this replaces and what
                // [`Row::value`] documents about duplicate names.
                m.entry(Box::from(c.name())).or_insert(i);
            }
            m
        });
        Header { columns, index }
    }

    /// The columns, in order.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        match &self.index {
            Some(m) => m.get(name).copied(),
            None => self.columns.iter().position(|c| c.name == name),
        }
    }
}

/// One decoded row: a shared column header and one [`Value`] per column.
///
/// Owned, cloneable, lifetime-free and driver-free — a backend converts its
/// native row once, at the seam, and everything above works on this. That is
/// what makes one [`FromRow`] impl correct on every backend (the per-engine
/// text forms are absorbed below, by `FromValue`'s documented text
/// acceptance), and what makes row mapping testable without a database.
#[derive(Debug, Clone)]
pub struct Row {
    header: Arc<Header>,
    values: Vec<Value>,
}

impl Row {
    /// Assemble a row that carries its own header.
    ///
    /// For a row on its own — a test, a hand-built fixture. A backend reading
    /// a *result set* builds one [`Header`] when the first row arrives and
    /// calls [`with_header`](Self::with_header) for each row, so the name
    /// lookup is prepared once rather than per row: a header assembled here
    /// belongs to one row and gets the scan, because an index built for a
    /// single row is pure cost.
    pub fn new(columns: Arc<[Column]>, values: Vec<Value>) -> Self {
        debug_assert_eq!(columns.len(), values.len());
        Row {
            header: Arc::new(Header {
                columns,
                index: None,
            }),
            values,
        }
    }

    /// Assemble a row against a header shared with the rest of its result
    /// set. Backend-facing.
    pub fn with_header(header: Arc<Header>, values: Vec<Value>) -> Self {
        debug_assert_eq!(header.columns.len(), values.len());
        Row { header, values }
    }

    /// The header this row shares with its result set.
    pub fn header(&self) -> &Arc<Header> {
        &self.header
    }

    /// The result set's columns, in order.
    pub fn columns(&self) -> &[Column] {
        &self.header.columns
    }

    /// The raw value under `name`, if such a column exists.
    ///
    /// Duplicate column names resolve to the first, documented and matching
    /// sqlx; reach the rest by position.
    pub fn value(&self, name: &str) -> Option<&Value> {
        self.index_of(name).map(|i| &self.values[i])
    }

    /// Read the column `name` as `T`. Clones the value; derived mappers use
    /// [`take`](Self::take) instead so `String`/`Vec<u8>`/JSON move.
    pub fn get<T: FromValue>(&self, name: &str) -> Result<T, ExecError> {
        let i = self.require(name)?;
        decode(&self.header.columns[i].name, self.values[i].clone())
    }

    /// Read the column at `index` as `T`.
    pub fn get_at<T: FromValue>(&self, index: usize) -> Result<T, ExecError> {
        let v = self.value_at(index)?.clone();
        decode(&positional_label(index, self.columns()), v)
    }

    /// Take the column `name` out of the row as `T`, leaving `NULL` behind.
    ///
    /// The consuming variant derived/generated `FromRow` impls use: `String`,
    /// `Vec<u8>` and JSON documents move rather than clone.
    pub fn take<T: FromValue>(&mut self, name: &str) -> Result<T, ExecError> {
        let i = self.require(name)?;
        let v = std::mem::replace(&mut self.values[i], Value::Null);
        decode(&self.header.columns[i].name, v)
    }

    /// Take the column at `index` out of the row as `T`.
    pub fn take_at<T: FromValue>(&mut self, index: usize) -> Result<T, ExecError> {
        if index >= self.values.len() {
            return Err(missing(&format!("#{index}"), self.columns()));
        }
        let v = std::mem::replace(&mut self.values[index], Value::Null);
        decode(&positional_label(index, self.columns()), v)
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.header.index_of(name)
    }

    fn require(&self, name: &str) -> Result<usize, ExecError> {
        self.index_of(name)
            .ok_or_else(|| missing(name, self.columns()))
    }

    fn value_at(&self, index: usize) -> Result<&Value, ExecError> {
        self.values
            .get(index)
            .ok_or_else(|| missing(&format!("#{index}"), self.columns()))
    }
}

/// The error label for positional access: the real name when the header has
/// one, `#N` otherwise.
fn positional_label(index: usize, columns: &[Column]) -> String {
    match columns.get(index) {
        Some(c) if !c.name.is_empty() => c.name.clone(),
        _ => format!("#{index}"),
    }
}

fn missing(column: &str, columns: &[Column]) -> ExecError {
    ExecError::MissingColumn {
        column: column.to_owned(),
        available: columns.iter().map(|c| c.name.clone()).collect(),
    }
}

/// Weave the column name into a `FromValue` failure — this is the boundary
/// where "cannot read NULL as String" becomes "column \"email\": cannot read
/// NULL as String".
fn decode<T: FromValue>(column: &str, v: Value) -> Result<T, ExecError> {
    T::from_value(v).map_err(|source| ExecError::Decode {
        column: column.to_owned(),
        source,
    })
}

/// A type that can be built from a whole row.
///
/// By name for structs (survives column reordering and `SELECT *` drift; the
/// mapping generated models will use), by position for tuples (quick ad-hoc
/// reads). The hand-written shape — the same one code generation will emit —
/// is one [`take`](Row::take) per field:
///
/// ```
/// use keelson_exec::{ExecError, FromRow, Row};
///
/// struct User {
///     id: i64,
///     email: Option<String>, // a nullable column must be an Option
/// }
///
/// impl FromRow for User {
///     fn from_row(row: &mut Row) -> Result<Self, ExecError> {
///         Ok(User {
///             id: row.take("id")?,
///             email: row.take("email")?,
///         })
///     }
/// }
/// ```
pub trait FromRow: Sized {
    /// Build `Self` out of `row`. Takes `&mut` so values move, never clone.
    fn from_row(row: &mut Row) -> Result<Self, ExecError>;
}

/// A row reads as itself, so `fetch_all::<Row>` hands back raw rows.
impl FromRow for Row {
    fn from_row(row: &mut Row) -> Result<Self, ExecError> {
        Ok(row.clone())
    }
}

macro_rules! from_row_tuple {
    ($($idx:tt : $t:ident),+) => {
        /// Positional: element `N` reads column `N`.
        impl<$($t: FromValue),+> FromRow for ($($t,)+) {
            fn from_row(row: &mut Row) -> Result<Self, ExecError> {
                Ok(($(row.take_at::<$t>($idx)?,)+))
            }
        }
    };
}

from_row_tuple!(0: A);
from_row_tuple!(0: A, 1: B);
from_row_tuple!(0: A, 1: B, 2: C);
from_row_tuple!(0: A, 1: B, 2: C, 3: D);
from_row_tuple!(0: A, 1: B, 2: C, 3: D, 4: E);
from_row_tuple!(0: A, 1: B, 2: C, 3: D, 4: E, 5: F);
from_row_tuple!(0: A, 1: B, 2: C, 3: D, 4: E, 5: F, 6: G);
from_row_tuple!(0: A, 1: B, 2: C, 3: D, 4: E, 5: F, 6: G, 7: H);

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Row {
        let columns: Arc<[Column]> =
            vec![Column::new("id"), Column::new("name"), Column::new("email")].into();
        Row::new(
            columns,
            vec![Value::I64(7), Value::Text("ada".into()), Value::Null],
        )
    }

    /// A header wide enough to take the prepared-map path, so every
    /// invariant below is checked on *both* sides of [`SCAN_LIMIT`] — the map
    /// is an optimisation and must be invisible.
    fn wide_row() -> Row {
        let names: Vec<String> = (0..SCAN_LIMIT + 4).map(|i| format!("c{i}")).collect();
        let header = Arc::new(Header::new(
            names.iter().map(Column::new).collect::<Vec<_>>(),
        ));
        let values = (0..names.len()).map(|i| Value::I64(i as i64)).collect();
        Row::with_header(header, values)
    }

    #[test]
    fn a_wide_header_resolves_names_through_the_prepared_map() {
        let r = wide_row();
        assert!(r.header().index.is_some(), "the map path is what is tested");
        for (i, c) in r.columns().iter().enumerate() {
            let name = c.name().to_owned();
            assert_eq!(r.get::<i64>(&name).unwrap(), i as i64);
        }
        assert_eq!(r.value("nope"), None);
    }

    #[test]
    fn a_narrow_header_scans() {
        assert!(row().header().index.is_none());
    }

    /// Duplicate names resolve to the first, and the map must agree with the
    /// scan it replaces — `HashMap` insertion would otherwise keep the last.
    #[test]
    fn a_duplicate_name_resolves_to_the_first_either_way() {
        for extra in [0, SCAN_LIMIT] {
            let mut names = vec!["dup".to_owned(), "dup".to_owned()];
            names.extend((0..extra).map(|i| format!("c{i}")));
            let header = Arc::new(Header::new(
                names.iter().map(Column::new).collect::<Vec<_>>(),
            ));
            let mut values = vec![Value::I64(1), Value::I64(2)];
            values.extend((0..extra).map(|_| Value::Null));
            let r = Row::with_header(header, values);
            assert_eq!(
                r.value("dup"),
                Some(&Value::I64(1)),
                "{} columns",
                2 + extra
            );
        }
    }

    /// A header assembled per row keeps its own columns, so `Row::new` stays
    /// the plain way to build one.
    #[test]
    fn a_row_built_on_its_own_carries_its_header() {
        let r = row();
        assert_eq!(r.header().columns().len(), 3);
        assert_eq!(r.columns()[0].name(), "id");
    }

    #[test]
    fn get_by_name_and_position() {
        let r = row();
        assert_eq!(r.get::<i64>("id").unwrap(), 7);
        assert_eq!(r.get_at::<String>(1).unwrap(), "ada");
        assert_eq!(r.get::<Option<String>>("email").unwrap(), None);
        assert_eq!(r.value("id"), Some(&Value::I64(7)));
        assert_eq!(r.value("nope"), None);
    }

    #[test]
    fn take_moves_the_value_out() {
        let mut r = row();
        assert_eq!(r.take::<String>("name").unwrap(), "ada");
        // Taken: what is left behind is NULL.
        assert_eq!(r.value("name"), Some(&Value::Null));
    }

    #[test]
    fn null_into_non_option_names_the_column() {
        let r = row();
        let e = r.get::<String>("email").unwrap_err();
        assert_eq!(
            e.to_string(),
            "column \"email\": cannot read NULL as String"
        );
    }

    #[test]
    fn type_mismatch_names_the_column_even_positionally() {
        let r = row();
        let e = r.get_at::<i64>(1).unwrap_err();
        assert_eq!(e.to_string(), "column \"name\": cannot read text as i64");
    }

    #[test]
    fn missing_column_lists_the_available_ones() {
        let r = row();
        let e = r.get::<i64>("emial").unwrap_err();
        assert_eq!(
            e.to_string(),
            "no column \"emial\" in result set (columns: id, name, email)"
        );
        let e = r.get_at::<i64>(9).unwrap_err();
        assert!(e.to_string().contains("#9"));
    }

    #[test]
    fn tuples_read_positionally() {
        let mut r = row();
        let (id, name, email) = <(i64, String, Option<String>)>::from_row(&mut r).unwrap();
        assert_eq!((id, name.as_str(), email), (7, "ada", None));
    }

    #[test]
    fn a_struct_maps_by_name_with_the_documented_pattern() {
        struct User {
            id: i64,
            name: String,
            email: Option<String>,
        }
        impl FromRow for User {
            fn from_row(row: &mut Row) -> Result<Self, ExecError> {
                Ok(User {
                    id: row.take("id")?,
                    name: row.take("name")?,
                    email: row.take("email")?,
                })
            }
        }
        let u = User::from_row(&mut row()).unwrap();
        assert_eq!((u.id, u.name.as_str(), u.email), (7, "ada", None));
    }
}
