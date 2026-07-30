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
    /// Escape hatch for dialect-specific types. Backends downcast through
    /// [`CustomValue::as_any`].
    Custom(Arc<dyn CustomValue>),
}

/// A dialect-specific value that keelson itself never interprets.
///
/// Implementors are carried through the builder untouched and handed to the
/// backend, which recovers the concrete type with [`Self::as_any`]. This is where
/// `uuid`, `chrono` and `serde_json` values live, so that core needs no optional
/// dependency on any of them.
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
/// shape `keelson-golden` compares against.
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
        // This is exactly the shape keelson-golden compares against.
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
}
