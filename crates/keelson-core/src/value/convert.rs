//! Rust type ⇄ [`Value`]: the two conversion traits and their impls.
//!
//! The two traits themselves stay in [`value`](super), because rustc names a
//! trait by where it is *defined*: moving `ToValue` down here made an
//! unsatisfied bound read `keelson_core::value::convert::ToValue` and teach
//! the reader a private path. What is here is the impls — one per width of
//! integer, per string type, per optional feature's type — which is the
//! length without the interface.

use crate::error::{Error, Result};
use crate::value::{CustomValue, FromValue, ToValue, Value};
use std::sync::Arc;

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
