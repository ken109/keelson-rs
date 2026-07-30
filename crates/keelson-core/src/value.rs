use std::any::Any;
use std::fmt;
use std::sync::Arc;

use serde::ser::{SerializeSeq, Serializer};

use crate::error::{Error, Result};

/// A bound argument.
///
/// keelson carries its own value enum rather than being generic over a driver's
/// parameter type. That keeps [`Expression`](crate::Expression) free of any
/// backend type parameter, and it means a built query's arguments can be
/// inspected — printed, compared, serialised — without a database in the loop.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// A boolean.
    Bool(bool),
    /// An 8-bit signed integer.
    I8(i8),
    /// A 16-bit signed integer.
    I16(i16),
    /// A 32-bit signed integer.
    I32(i32),
    /// A 64-bit signed integer.
    I64(i64),
    /// An 8-bit unsigned integer.
    U8(u8),
    /// A 16-bit unsigned integer.
    U16(u16),
    /// A 32-bit unsigned integer.
    U32(u32),
    /// A 64-bit unsigned integer.
    U64(u64),
    /// A single-precision float.
    F32(f32),
    /// A double-precision float.
    F64(f64),
    /// Character data.
    Text(String),
    /// Binary data — `BYTEA`, `BLOB`.
    Bytes(Vec<u8>),
    /// A homogeneous array, for dialects that have one (PostgreSQL).
    Array(Vec<Value>),
    /// A calendar date with no time and no zone — `DATE`.
    #[cfg(feature = "chrono")]
    Date(chrono::NaiveDate),
    /// A wall-clock time with no date and no zone — `TIME`.
    #[cfg(feature = "chrono")]
    Time(chrono::NaiveTime),
    /// A date and time with no zone — `TIMESTAMP` / `DATETIME`. What it means
    /// depends on context the database never sees, which is why it is a
    /// different variant from [`Value::TimestampTz`], not a special case of it.
    #[cfg(feature = "chrono")]
    DateTime(chrono::NaiveDateTime),
    /// An instant, carried in UTC — `TIMESTAMPTZ`.
    ///
    /// There is deliberately no offset-preserving variant: every zoned
    /// `chrono::DateTime<Tz>` is normalised to UTC on conversion, because no
    /// target database round-trips an offset (PostgreSQL's `timestamptz`
    /// stores UTC and renders in the session zone; MySQL's `TIMESTAMP`
    /// converts through the session zone; SQLite has no zone at all). A
    /// variant that pretended otherwise would promise what no backend keeps.
    #[cfg(feature = "chrono")]
    TimestampTz(chrono::DateTime<chrono::Utc>),
    /// A UUID — `uuid` on PostgreSQL, hyphenated text elsewhere.
    #[cfg(feature = "uuid")]
    Uuid(uuid::Uuid),
    /// An exact decimal number — `NUMERIC` / `DECIMAL`. A separate variant
    /// from the floats because binary floating point cannot represent decimal
    /// scale, which is the entire reason an application reaches for `Decimal`.
    #[cfg(feature = "decimal")]
    Decimal(rust_decimal::Decimal),
    /// A JSON document — `jsonb` / `JSON`, serialised text elsewhere.
    #[cfg(feature = "json")]
    Json(serde_json::Value),
    /// Escape hatch for dialect-specific types. Backends downcast through
    /// [`CustomValue::as_any`].
    Custom(Arc<dyn CustomValue>),
}

/// A dialect-specific value that keelson itself never interprets.
///
/// Implementors are carried through the builder untouched and handed to the
/// backend, which recovers the concrete type with [`Self::as_any`]. This is where
/// genuinely dialect-specific types live — PostgreSQL ranges, geometric types
/// and the like. The types nearly every application binds (`chrono`, `uuid`,
/// `rust_decimal`, `serde_json`) have first-class feature-gated variants
/// instead, with their mappings recorded in `docs/type-mappings.md`.
pub trait CustomValue: fmt::Debug + Send + Sync + 'static {
    /// The name used in error messages and `Debug` output.
    fn type_name(&self) -> &'static str;

    /// For downcasting in a backend adapter.
    fn as_any(&self) -> &dyn Any;

    /// A plain stand-in used when the argument list is serialised, e.g. by a
    /// logger or a golden test. Returning another [`Value::Custom`] serialises
    /// as `null`; there is no recursion.
    fn to_plain(&self) -> Value {
        Value::Null
    }
}

impl Value {
    /// Build a [`Value::Array`] from anything iterable.
    ///
    /// There is deliberately no blanket `ToValue for Vec<T>`: it would collide
    /// with `Vec<u8>`, which must stay [`Value::Bytes`] so `BYTEA`/`BLOB` binds
    /// correctly. Arrays are therefore explicit.
    pub fn array<T: ToValue, I: IntoIterator<Item = T>>(items: I) -> Value {
        Value::Array(items.into_iter().map(ToValue::to_value).collect())
    }

    /// Wrap a dialect-specific value.
    pub fn custom<C: CustomValue>(value: C) -> Value {
        Value::Custom(Arc::new(value))
    }

    /// Whether this is [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The variant name, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "NULL",
            Value::Bool(_) => "bool",
            Value::I8(_) => "i8",
            Value::I16(_) => "i16",
            Value::I32(_) => "i32",
            Value::I64(_) => "i64",
            Value::U8(_) => "u8",
            Value::U16(_) => "u16",
            Value::U32(_) => "u32",
            Value::U64(_) => "u64",
            Value::F32(_) => "f32",
            Value::F64(_) => "f64",
            Value::Text(_) => "text",
            Value::Bytes(_) => "bytes",
            Value::Array(_) => "array",
            #[cfg(feature = "chrono")]
            Value::Date(_) => "date",
            #[cfg(feature = "chrono")]
            Value::Time(_) => "time",
            #[cfg(feature = "chrono")]
            Value::DateTime(_) => "datetime",
            #[cfg(feature = "chrono")]
            Value::TimestampTz(_) => "timestamptz",
            #[cfg(feature = "uuid")]
            Value::Uuid(_) => "uuid",
            #[cfg(feature = "decimal")]
            Value::Decimal(_) => "decimal",
            #[cfg(feature = "json")]
            Value::Json(_) => "json",
            Value::Custom(c) => c.type_name(),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        use Value::*;
        match (self, other) {
            (Null, Null) => true,
            (Bool(a), Bool(b)) => a == b,
            (I8(a), I8(b)) => a == b,
            (I16(a), I16(b)) => a == b,
            (I32(a), I32(b)) => a == b,
            (I64(a), I64(b)) => a == b,
            (U8(a), U8(b)) => a == b,
            (U16(a), U16(b)) => a == b,
            (U32(a), U32(b)) => a == b,
            (U64(a), U64(b)) => a == b,
            (F32(a), F32(b)) => a == b,
            (F64(a), F64(b)) => a == b,
            (Text(a), Text(b)) => a == b,
            (Bytes(a), Bytes(b)) => a == b,
            (Array(a), Array(b)) => a == b,
            #[cfg(feature = "chrono")]
            (Date(a), Date(b)) => a == b,
            #[cfg(feature = "chrono")]
            (Time(a), Time(b)) => a == b,
            #[cfg(feature = "chrono")]
            (DateTime(a), DateTime(b)) => a == b,
            #[cfg(feature = "chrono")]
            (TimestampTz(a), TimestampTz(b)) => a == b,
            #[cfg(feature = "uuid")]
            (Uuid(a), Uuid(b)) => a == b,
            // rust_decimal compares numerically, so `1.10 == 1.100` here even
            // though the two serialise differently. That is the right call for
            // an argument list: the database would treat them as equal too.
            #[cfg(feature = "decimal")]
            (Decimal(a), Decimal(b)) => a == b,
            #[cfg(feature = "json")]
            (Json(a), Json(b)) => a == b,
            // Custom values have no shared notion of equality, so identity is
            // the only honest answer.
            (Custom(a), Custom(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// `Value` serialises as the underlying scalar, never as a tagged enum.
///
/// This is what makes `Vec<Value>` comparable against a plain JSON array of
/// arguments — `Value::I32(100)` becomes `100`, not `{"I32":100}` — which is the
/// shape the test suite compares against.
impl serde::Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Value::Null => s.serialize_none(),
            Value::Bool(v) => s.serialize_bool(*v),
            Value::I8(v) => s.serialize_i8(*v),
            Value::I16(v) => s.serialize_i16(*v),
            Value::I32(v) => s.serialize_i32(*v),
            Value::I64(v) => s.serialize_i64(*v),
            Value::U8(v) => s.serialize_u8(*v),
            Value::U16(v) => s.serialize_u16(*v),
            Value::U32(v) => s.serialize_u32(*v),
            Value::U64(v) => s.serialize_u64(*v),
            Value::F32(v) => s.serialize_f32(*v),
            Value::F64(v) => s.serialize_f64(*v),
            Value::Text(v) => s.serialize_str(v),
            Value::Bytes(v) => s.serialize_bytes(v),
            Value::Array(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            // The temporal types serialise as ISO 8601 strings — the one
            // rendering every dialect, log reader and JSON consumer agrees on.
            // Fractional seconds appear only when non-zero, in 3/6/9-digit
            // groups, so a whole-second timestamp stays short. The exact forms
            // are pinned in docs/type-mappings.md and by test.
            #[cfg(feature = "chrono")]
            Value::Date(v) => s.collect_str(&v.format("%Y-%m-%d")),
            #[cfg(feature = "chrono")]
            Value::Time(v) => s.collect_str(&v.format("%H:%M:%S%.f")),
            #[cfg(feature = "chrono")]
            Value::DateTime(v) => s.collect_str(&v.format("%Y-%m-%dT%H:%M:%S%.f")),
            #[cfg(feature = "chrono")]
            Value::TimestampTz(v) => {
                s.collect_str(&v.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
            }
            // Hyphenated lowercase — the RFC 9562 text form.
            #[cfg(feature = "uuid")]
            Value::Uuid(v) => s.collect_str(v),
            // A string, never a JSON number: `1.10` as a float would collapse
            // to `1.1` (or worse), and exactness is what `Decimal` is for.
            #[cfg(feature = "decimal")]
            Value::Decimal(v) => s.collect_str(v),
            // Structural passthrough, like `Array` — the document itself, not a
            // string containing it.
            #[cfg(feature = "json")]
            Value::Json(v) => v.serialize(s),
            Value::Custom(c) => match c.to_plain() {
                Value::Custom(_) => s.serialize_none(),
                plain => plain.serialize(s),
            },
        }
    }
}

/// Conversion into a bound argument.
pub trait ToValue {
    /// Consume `self` and produce the argument to bind.
    fn to_value(self) -> Value;
}

impl ToValue for Value {
    fn to_value(self) -> Value {
        self
    }
}

/// `None` binds as SQL `NULL`.
impl<T: ToValue> ToValue for Option<T> {
    fn to_value(self) -> Value {
        match self {
            Some(v) => v.to_value(),
            None => Value::Null,
        }
    }
}

macro_rules! to_value_direct {
    ($($t:ty => $variant:ident),* $(,)?) => { $(
        impl ToValue for $t {
            fn to_value(self) -> Value {
                Value::$variant(self)
            }
        }
    )* };
}

to_value_direct! {
    bool => Bool,
    i8 => I8, i16 => I16, i32 => I32, i64 => I64,
    u8 => U8, u16 => U16, u32 => U32, u64 => U64,
    f32 => F32, f64 => F64,
    String => Text,
    Vec<u8> => Bytes,
}

impl ToValue for &str {
    fn to_value(self) -> Value {
        Value::Text(self.to_owned())
    }
}

impl ToValue for &String {
    fn to_value(self) -> Value {
        Value::Text(self.clone())
    }
}

impl ToValue for std::borrow::Cow<'_, str> {
    fn to_value(self) -> Value {
        Value::Text(self.into_owned())
    }
}

impl ToValue for &[u8] {
    fn to_value(self) -> Value {
        Value::Bytes(self.to_vec())
    }
}

// Pointer-width integers are normalised so a backend never has to branch on the
// host architecture.
impl ToValue for isize {
    fn to_value(self) -> Value {
        Value::I64(self as i64)
    }
}

impl ToValue for usize {
    fn to_value(self) -> Value {
        Value::U64(self as u64)
    }
}

/// The unit type binds as `NULL`, so `push_arg(())` needs no ceremony.
impl ToValue for () {
    fn to_value(self) -> Value {
        Value::Null
    }
}

impl<T: CustomValue> ToValue for Arc<T> {
    fn to_value(self) -> Value {
        Value::Custom(self)
    }
}

#[cfg(feature = "chrono")]
mod chrono_impls {
    use super::{FromValue, ToValue, Value};
    use crate::error::{Error, Result};
    use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

    impl ToValue for NaiveDate {
        fn to_value(self) -> Value {
            Value::Date(self)
        }
    }

    impl ToValue for NaiveTime {
        fn to_value(self) -> Value {
            Value::Time(self)
        }
    }

    impl ToValue for NaiveDateTime {
        fn to_value(self) -> Value {
            Value::DateTime(self)
        }
    }

    /// Any zoned datetime — `Utc`, `FixedOffset`, `Local` — binds as the
    /// instant it names, normalised to UTC. The offset is dropped because no
    /// target database stores one; an application that needs the original
    /// offset keeps it in its own column.
    impl<Tz: TimeZone> ToValue for DateTime<Tz> {
        fn to_value(self) -> Value {
            Value::TimestampTz(self.with_timezone(&Utc))
        }
    }

    // Reading back accepts the matching variant or its ISO 8601 text form,
    // because SQLite has no temporal storage class at all and MySQL drivers
    // routinely hand temporal columns back as text. The text forms accepted
    // are exactly the ones `Value` serialises to (docs/type-mappings.md),
    // plus the space-separated datetime that SQLite and MySQL conventionally
    // store, so a value written through keelson always reads back.

    impl FromValue for NaiveDate {
        fn from_value(v: Value) -> Result<Self> {
            let found = v.type_name();
            match v {
                Value::Date(d) => Ok(d),
                Value::Text(s) => s
                    .parse()
                    .map_err(|_| Error::type_mismatch("NaiveDate", found)),
                _ => Err(Error::type_mismatch("NaiveDate", found)),
            }
        }
    }

    impl FromValue for NaiveTime {
        fn from_value(v: Value) -> Result<Self> {
            let found = v.type_name();
            match v {
                Value::Time(t) => Ok(t),
                Value::Text(s) => s
                    .parse()
                    .map_err(|_| Error::type_mismatch("NaiveTime", found)),
                _ => Err(Error::type_mismatch("NaiveTime", found)),
            }
        }
    }

    impl FromValue for NaiveDateTime {
        fn from_value(v: Value) -> Result<Self> {
            let found = v.type_name();
            match v {
                Value::DateTime(dt) => Ok(dt),
                Value::Text(s) => s
                    .parse()
                    .or_else(|_| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S%.f"))
                    .map_err(|_| Error::type_mismatch("NaiveDateTime", found)),
                _ => Err(Error::type_mismatch("NaiveDateTime", found)),
            }
        }
    }

    impl FromValue for DateTime<Utc> {
        fn from_value(v: Value) -> Result<Self> {
            let found = v.type_name();
            match v {
                Value::TimestampTz(dt) => Ok(dt),
                Value::Text(s) => DateTime::parse_from_rfc3339(&s)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|_| Error::type_mismatch("DateTime<Utc>", found)),
                _ => Err(Error::type_mismatch("DateTime<Utc>", found)),
            }
        }
    }
}

#[cfg(feature = "uuid")]
mod uuid_impls {
    use super::{FromValue, ToValue, Value};
    use crate::error::{Error, Result};
    use uuid::Uuid;

    impl ToValue for Uuid {
        fn to_value(self) -> Value {
            Value::Uuid(self)
        }
    }

    impl FromValue for Uuid {
        fn from_value(v: Value) -> Result<Self> {
            let found = v.type_name();
            match v {
                Value::Uuid(u) => Ok(u),
                // Text covers the standard MySQL/SQLite mapping (`CHAR(36)` /
                // `TEXT`); 16 raw bytes covers a `BINARY(16)`/`BLOB` column an
                // application chose for compactness.
                Value::Text(s) => {
                    Uuid::parse_str(&s).map_err(|_| Error::type_mismatch("Uuid", found))
                }
                Value::Bytes(b) => {
                    Uuid::from_slice(&b).map_err(|_| Error::type_mismatch("Uuid", found))
                }
                _ => Err(Error::type_mismatch("Uuid", found)),
            }
        }
    }
}

#[cfg(feature = "decimal")]
mod decimal_impls {
    use super::{FromValue, ToValue, Value};
    use crate::error::{Error, Result};
    use rust_decimal::Decimal;

    impl ToValue for Decimal {
        fn to_value(self) -> Value {
            Value::Decimal(self)
        }
    }

    impl FromValue for Decimal {
        fn from_value(v: Value) -> Result<Self> {
            let found = v.type_name();
            // Text covers drivers that hand `NUMERIC` back as a string (the
            // lossless wire form) and the SQLite `TEXT` mapping; integers are
            // exact so they widen in. Floats are deliberately rejected: a
            // binary fraction has no faithful decimal scale, and inventing one
            // silently is the bug `Decimal` exists to prevent.
            match v {
                Value::Decimal(d) => Ok(d),
                Value::Text(s) => s
                    .parse()
                    .map_err(|_| Error::type_mismatch("Decimal", found)),
                Value::I8(x) => Ok(Decimal::from(x)),
                Value::I16(x) => Ok(Decimal::from(x)),
                Value::I32(x) => Ok(Decimal::from(x)),
                Value::I64(x) => Ok(Decimal::from(x)),
                Value::U8(x) => Ok(Decimal::from(x)),
                Value::U16(x) => Ok(Decimal::from(x)),
                Value::U32(x) => Ok(Decimal::from(x)),
                Value::U64(x) => Ok(Decimal::from(x)),
                _ => Err(Error::type_mismatch("Decimal", found)),
            }
        }
    }
}

#[cfg(feature = "json")]
mod json_impls {
    use super::{FromValue, ToValue, Value};
    use crate::error::{Error, Result};

    impl ToValue for serde_json::Value {
        fn to_value(self) -> Value {
            Value::Json(self)
        }
    }

    impl FromValue for serde_json::Value {
        fn from_value(v: Value) -> Result<Self> {
            let found = v.type_name();
            match v {
                Value::Json(j) => Ok(j),
                // Every dialect's JSON type comes off the wire as text in at
                // least one driver, so parseable text reads as the document.
                Value::Text(s) => serde_json::from_str(&s)
                    .map_err(|_| Error::type_mismatch("serde_json::Value", found)),
                _ => Err(Error::type_mismatch("serde_json::Value", found)),
            }
        }
    }
}

/// Conversion out of a value read back from the database.
pub trait FromValue: Sized {
    /// Consume a [`Value`] and produce `Self`, or explain why not.
    fn from_value(v: Value) -> Result<Self>;
}

impl FromValue for Value {
    fn from_value(v: Value) -> Result<Self> {
        Ok(v)
    }
}

/// `NULL` reads as `None`; anything else delegates to `T`.
impl<T: FromValue> FromValue for Option<T> {
    fn from_value(v: Value) -> Result<Self> {
        match v {
            Value::Null => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }
}

macro_rules! from_value_int {
    ($($t:ty),* $(,)?) => { $(
        impl FromValue for $t {
            fn from_value(v: Value) -> Result<Self> {
                let found = v.type_name();
                // A driver is free to hand back any integer width, so widening
                // is accepted and only a genuine overflow is an error.
                let converted = match v {
                    Value::I8(x) => <$t>::try_from(x).ok(),
                    Value::I16(x) => <$t>::try_from(x).ok(),
                    Value::I32(x) => <$t>::try_from(x).ok(),
                    Value::I64(x) => <$t>::try_from(x).ok(),
                    Value::U8(x) => <$t>::try_from(x).ok(),
                    Value::U16(x) => <$t>::try_from(x).ok(),
                    Value::U32(x) => <$t>::try_from(x).ok(),
                    Value::U64(x) => <$t>::try_from(x).ok(),
                    _ => return Err(Error::type_mismatch(stringify!($t), found)),
                };
                converted.ok_or(Error::type_mismatch(stringify!($t), found))
            }
        }
    )* };
}

from_value_int!(i8, i16, i32, i64, u8, u16, u32, u64);

macro_rules! from_value_float {
    ($($t:ty),* $(,)?) => { $(
        impl FromValue for $t {
            #[allow(clippy::cast_lossless, clippy::cast_precision_loss)]
            fn from_value(v: Value) -> Result<Self> {
                let found = v.type_name();
                match v {
                    Value::F32(x) => Ok(x as $t),
                    Value::F64(x) => Ok(x as $t),
                    Value::I8(x) => Ok(x as $t),
                    Value::I16(x) => Ok(x as $t),
                    Value::I32(x) => Ok(x as $t),
                    Value::I64(x) => Ok(x as $t),
                    Value::U8(x) => Ok(x as $t),
                    Value::U16(x) => Ok(x as $t),
                    Value::U32(x) => Ok(x as $t),
                    Value::U64(x) => Ok(x as $t),
                    _ => Err(Error::type_mismatch(stringify!($t), found)),
                }
            }
        }
    )* };
}

from_value_float!(f32, f64);

impl FromValue for bool {
    fn from_value(v: Value) -> Result<Self> {
        match v {
            Value::Bool(b) => Ok(b),
            other => Err(Error::type_mismatch("bool", other.type_name())),
        }
    }
}

impl FromValue for String {
    fn from_value(v: Value) -> Result<Self> {
        match v {
            Value::Text(s) => Ok(s),
            other => Err(Error::type_mismatch("String", other.type_name())),
        }
    }
}

impl FromValue for Vec<u8> {
    fn from_value(v: Value) -> Result<Self> {
        match v {
            Value::Bytes(b) => Ok(b),
            // MySQL hands back character columns as bytes and vice versa, so
            // this direction is always safe.
            Value::Text(s) => Ok(s.into_bytes()),
            other => Err(Error::type_mismatch("Vec<u8>", other.type_name())),
        }
    }
}

/// Read a [`Value::Array`] element-wise.
///
/// A free function rather than `impl FromValue for Vec<T>`, which would collide
/// with `Vec<u8>` = [`Value::Bytes`]. Same asymmetry as [`Value::array`], for the
/// same reason.
pub fn from_value_array<T: FromValue>(v: Value) -> Result<Vec<T>> {
    match v {
        Value::Array(items) => items.into_iter().map(T::from_value).collect(),
        other => Err(Error::type_mismatch("array", other.type_name())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Point(i32, i32);

    impl CustomValue for Point {
        fn type_name(&self) -> &'static str {
            "point"
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn to_plain(&self) -> Value {
            Value::Text(format!("({},{})", self.0, self.1))
        }
    }

    #[derive(Debug)]
    struct Opaque;

    impl CustomValue for Opaque {
        fn type_name(&self) -> &'static str {
            "opaque"
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    fn json(v: Value) -> serde_json::Value {
        serde_json::to_value(v).expect("Value must serialise")
    }

    #[test]
    fn serialises_as_the_bare_scalar_not_a_tagged_variant() {
        assert_eq!(json(Value::I32(100)), serde_json::json!(100));
        assert_eq!(json(Value::I64(-7)), serde_json::json!(-7));
        assert_eq!(json(Value::U8(3)), serde_json::json!(3));
        assert_eq!(json(Value::Text("100".into())), serde_json::json!("100"));
        assert_eq!(json(Value::Bool(true)), serde_json::json!(true));
        assert_eq!(json(Value::F64(1.5)), serde_json::json!(1.5));
        assert_eq!(json(Value::Null), serde_json::Value::Null);
    }

    #[test]
    fn serialises_a_whole_arg_list_as_a_plain_json_array() {
        // This is exactly the shape the test suite compares against.
        let args = vec![Value::I32(100), Value::Text("Stephen".into())];
        assert_eq!(
            serde_json::to_value(&args).unwrap(),
            serde_json::json!([100, "Stephen"])
        );
    }

    #[test]
    fn serialises_arrays_and_bytes_structurally() {
        assert_eq!(
            json(Value::array([1i32, 2, 3])),
            serde_json::json!([1, 2, 3])
        );
        assert_eq!(json(Value::Bytes(vec![1, 2])), serde_json::json!([1, 2]));
    }

    #[test]
    fn custom_values_serialise_through_their_plain_form() {
        assert_eq!(json(Value::custom(Point(1, 2))), serde_json::json!("(1,2)"));
        assert_eq!(json(Value::custom(Opaque)), serde_json::Value::Null);
    }

    #[test]
    fn custom_values_are_downcastable_by_a_backend() {
        let v = Value::custom(Point(3, 4));
        let Value::Custom(c) = &v else {
            panic!("expected a custom value");
        };
        let p = c.as_any().downcast_ref::<Point>().expect("downcast");
        assert_eq!((p.0, p.1), (3, 4));
        assert_eq!(v.type_name(), "point");
    }

    #[test]
    fn option_none_binds_as_null() {
        assert_eq!(None::<i32>.to_value(), Value::Null);
        assert_eq!(Some(4i32).to_value(), Value::I32(4));
        assert_eq!(Some("a").to_value(), Value::Text("a".into()));
        assert!(None::<String>.to_value().is_null());
    }

    #[test]
    fn to_value_covers_the_obvious_primitives() {
        assert_eq!(true.to_value(), Value::Bool(true));
        assert_eq!(1i16.to_value(), Value::I16(1));
        assert_eq!(1u32.to_value(), Value::U32(1));
        assert_eq!(1.5f32.to_value(), Value::F32(1.5));
        assert_eq!("x".to_value(), Value::Text("x".into()));
        assert_eq!(String::from("x").to_value(), Value::Text("x".into()));
        assert_eq!(
            std::borrow::Cow::Borrowed("x").to_value(),
            Value::Text("x".into())
        );
        assert_eq!(vec![1u8, 2].to_value(), Value::Bytes(vec![1, 2]));
        assert_eq!(9usize.to_value(), Value::U64(9));
        assert_eq!((-9isize).to_value(), Value::I64(-9));
        assert_eq!(().to_value(), Value::Null);
        assert_eq!(Value::I32(1).to_value(), Value::I32(1));
    }

    #[test]
    fn from_value_widens_and_rejects_overflow() {
        assert_eq!(i64::from_value(Value::I32(5)).unwrap(), 5);
        assert_eq!(u8::from_value(Value::I64(200)).unwrap(), 200);
        assert!(u8::from_value(Value::I64(300)).is_err());
        assert!(i32::from_value(Value::Text("3".into())).is_err());
        assert_eq!(f64::from_value(Value::I32(2)).unwrap(), 2.0);
        assert!(bool::from_value(Value::Bool(false)).unwrap().eq(&false));
        assert_eq!(String::from_value(Value::Text("s".into())).unwrap(), "s");
        assert_eq!(Option::<i32>::from_value(Value::Null).unwrap(), None);
        assert_eq!(
            from_value_array::<i32>(Value::array([1i32, 2])).unwrap(),
            vec![1, 2]
        );
        assert!(from_value_array::<i32>(Value::I32(1)).is_err());
    }

    #[test]
    fn type_mismatch_explains_both_sides() {
        let e = i32::from_value(Value::Text("3".into())).unwrap_err();
        assert_eq!(e.to_string(), "cannot read text as i32");
    }

    // Expected strings below are the ISO 8601 / RFC 3339 / RFC 9562 text forms
    // pinned in docs/type-mappings.md, written out by hand — not copied from
    // output.

    #[cfg(feature = "chrono")]
    mod chrono_values {
        use super::*;
        use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

        fn date() -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, 7, 30).unwrap()
        }

        fn time() -> NaiveTime {
            NaiveTime::from_hms_opt(12, 34, 56).unwrap()
        }

        #[test]
        fn to_value_wraps_each_temporal_type() {
            assert_eq!(date().to_value(), Value::Date(date()));
            assert_eq!(time().to_value(), Value::Time(time()));
            let dt = date().and_time(time());
            assert_eq!(dt.to_value(), Value::DateTime(dt));
            let utc = Utc.with_ymd_and_hms(2026, 7, 30, 12, 34, 56).unwrap();
            assert_eq!(utc.to_value(), Value::TimestampTz(utc));
        }

        #[test]
        fn zoned_datetimes_normalise_to_utc() {
            // 21:34:56+09:00 names the same instant as 12:34:56Z.
            let jst: DateTime<FixedOffset> = "2026-07-30T21:34:56+09:00".parse().unwrap();
            let utc = Utc.with_ymd_and_hms(2026, 7, 30, 12, 34, 56).unwrap();
            assert_eq!(jst.to_value(), Value::TimestampTz(utc));
        }

        #[test]
        fn serialises_as_iso_8601_strings() {
            assert_eq!(json(date().to_value()), serde_json::json!("2026-07-30"));
            assert_eq!(json(time().to_value()), serde_json::json!("12:34:56"));
            assert_eq!(
                json(date().and_time(time()).to_value()),
                serde_json::json!("2026-07-30T12:34:56")
            );
            let utc = Utc.with_ymd_and_hms(2026, 7, 30, 12, 34, 56).unwrap();
            assert_eq!(
                json(utc.to_value()),
                serde_json::json!("2026-07-30T12:34:56Z")
            );
        }

        #[test]
        fn fractional_seconds_appear_only_when_non_zero() {
            let t = NaiveTime::from_hms_milli_opt(12, 34, 56, 789).unwrap();
            assert_eq!(json(t.to_value()), serde_json::json!("12:34:56.789"));
            let dt = date().and_time(t);
            assert_eq!(
                json(dt.to_value()),
                serde_json::json!("2026-07-30T12:34:56.789")
            );
            let utc = DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc);
            assert_eq!(
                json(utc.to_value()),
                serde_json::json!("2026-07-30T12:34:56.789Z")
            );
        }

        #[test]
        fn round_trips_from_its_own_variant_and_serialised_text() {
            let utc = Utc.with_ymd_and_hms(2026, 7, 30, 12, 34, 56).unwrap();
            assert_eq!(NaiveDate::from_value(date().to_value()).unwrap(), date());
            assert_eq!(
                NaiveDate::from_value(Value::Text("2026-07-30".into())).unwrap(),
                date()
            );
            assert_eq!(
                NaiveTime::from_value(Value::Text("12:34:56".into())).unwrap(),
                time()
            );
            let dt = date().and_time(time());
            assert_eq!(
                NaiveDateTime::from_value(Value::Text("2026-07-30T12:34:56".into())).unwrap(),
                dt
            );
            // The space-separated form SQLite and MySQL conventionally store.
            assert_eq!(
                NaiveDateTime::from_value(Value::Text("2026-07-30 12:34:56".into())).unwrap(),
                dt
            );
            assert_eq!(DateTime::<Utc>::from_value(utc.to_value()).unwrap(), utc);
            assert_eq!(
                DateTime::<Utc>::from_value(Value::Text("2026-07-30T21:34:56+09:00".into()))
                    .unwrap(),
                utc
            );
            assert!(NaiveDate::from_value(Value::I32(1)).is_err());
            assert!(NaiveDate::from_value(Value::Text("not a date".into())).is_err());
        }

        #[test]
        fn type_names_are_reported() {
            assert_eq!(date().to_value().type_name(), "date");
            assert_eq!(time().to_value().type_name(), "time");
            assert_eq!(date().and_time(time()).to_value().type_name(), "datetime");
            let utc = Utc.with_ymd_and_hms(2026, 7, 30, 0, 0, 0).unwrap();
            assert_eq!(utc.to_value().type_name(), "timestamptz");
        }
    }

    #[cfg(feature = "uuid")]
    mod uuid_values {
        use super::*;
        use uuid::Uuid;

        const HYPHENATED: &str = "550e8400-e29b-41d4-a716-446655440000";

        #[test]
        fn binds_serialises_and_round_trips() {
            let u = Uuid::parse_str(HYPHENATED).unwrap();
            assert_eq!(u.to_value(), Value::Uuid(u));
            assert_eq!(u.to_value().type_name(), "uuid");
            assert_eq!(json(u.to_value()), serde_json::json!(HYPHENATED));
            assert_eq!(Uuid::from_value(u.to_value()).unwrap(), u);
            assert_eq!(Uuid::from_value(Value::Text(HYPHENATED.into())).unwrap(), u);
            assert_eq!(
                Uuid::from_value(Value::Bytes(u.as_bytes().to_vec())).unwrap(),
                u
            );
            assert!(Uuid::from_value(Value::Bytes(vec![1, 2, 3])).is_err());
            assert!(Uuid::from_value(Value::I32(1)).is_err());
        }
    }

    #[cfg(feature = "decimal")]
    mod decimal_values {
        use super::*;
        use rust_decimal::Decimal;

        #[test]
        fn binds_serialises_and_round_trips() {
            // 19.99 with an explicit scale of 2.
            let d = Decimal::new(1999, 2);
            assert_eq!(d.to_value(), Value::Decimal(d));
            assert_eq!(d.to_value().type_name(), "decimal");
            // A string, never a JSON number — exactness survives any reader.
            assert_eq!(json(d.to_value()), serde_json::json!("19.99"));
            assert_eq!(Decimal::from_value(d.to_value()).unwrap(), d);
            assert_eq!(Decimal::from_value(Value::Text("19.99".into())).unwrap(), d);
            assert_eq!(
                Decimal::from_value(Value::I64(7)).unwrap(),
                Decimal::from(7)
            );
            // Floats are rejected: no faithful decimal scale exists for them.
            assert!(Decimal::from_value(Value::F64(19.99)).is_err());
        }

        #[test]
        fn trailing_zeros_survive_serialisation() {
            // 1.10 keeps scale 2 — `NUMERIC` preserves scale, so keelson does.
            let d = Decimal::new(110, 2);
            assert_eq!(json(d.to_value()), serde_json::json!("1.10"));
            // ...while equality is numeric, like the database's.
            assert_eq!(d.to_value(), Decimal::new(11, 1).to_value());
        }
    }

    #[cfg(feature = "json")]
    mod json_values {
        use super::*;

        #[test]
        fn binds_serialises_structurally_and_round_trips() {
            let doc = serde_json::json!({"a": [1, 2], "b": "x"});
            assert_eq!(doc.clone().to_value(), Value::Json(doc.clone()));
            assert_eq!(doc.clone().to_value().type_name(), "json");
            // The document itself, not a string containing it.
            assert_eq!(json(doc.clone().to_value()), doc);
            assert_eq!(
                serde_json::Value::from_value(doc.clone().to_value()).unwrap(),
                doc
            );
            assert_eq!(
                serde_json::Value::from_value(Value::Text(r#"{"a":[1,2],"b":"x"}"#.into()))
                    .unwrap(),
                doc
            );
            assert!(serde_json::Value::from_value(Value::Text("not json".into())).is_err());
            assert!(serde_json::Value::from_value(Value::I32(1)).is_err());
        }
    }
}
