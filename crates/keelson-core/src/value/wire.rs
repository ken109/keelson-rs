//! `Value`'s wire format: the bare scalar, never a tagged enum.
//!
//! Its own file because it is a *contract*, not a mechanical impl. The
//! encodings below — ISO-8601 for dates, RFC-3339 for instants, RFC-9562 for
//! UUIDs, a decimal string for `Decimal` — are what a recorded argument list
//! is compared against, so changing one is a change to what every golden
//! comparison means.

use serde::ser::{SerializeSeq, Serializer};

use crate::value::Value;

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
