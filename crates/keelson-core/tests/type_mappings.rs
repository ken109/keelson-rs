//! The standard type mappings, exercised from outside the crate.
//!
//! One case per optional `Value` feature, each proving the contract that
//! docs/type-mappings.md records and that the execution layer and codegen will
//! code against:
//!
//! 1. the type binds through `expr::arg` with no wrapper — the whole point of
//!    the first-class variants over `CustomValue`;
//! 2. it renders as the dialect's placeholder, never inline — `Value` has no
//!    literal rendering, so a mapped type cannot leak into the SQL text;
//! 3. its `Serialize` form is the pinned bare scalar, so a bound argument list
//!    still compares as a plain JSON array.
//!
//! Placeholders render under [`Numbered`] (`$N`) because a positional `?`
//! renders the same string no matter how the counter is wrong. Expected JSON is
//! written out by hand from the ISO 8601 / RFC 3339 / RFC 9562 text forms —
//! not copied from output.
//!
//! Run with the features on to reach these cases:
//!
//!     cargo test -p keelson-core --features "chrono uuid decimal json"

#![cfg(any(
    feature = "chrono",
    feature = "uuid",
    feature = "decimal",
    feature = "json"
))]

use keelson_core::build;
use keelson_core::expr::{Chain, arg, quote};
use keelson_core::testing::Numbered;

/// Render `"col" = arg(v)` and hand back the SQL and the serialised args.
fn bind_one(col: &'static str, v: impl keelson_core::ToValue) -> (String, serde_json::Value) {
    let e = quote(col).eq(arg(v));
    let (sql, args) = build(&Numbered, &e).expect("render");
    let args = serde_json::to_value(&args).expect("args must serialise");
    (sql, args)
}

#[cfg(feature = "chrono")]
mod chrono_binds {
    use super::*;
    use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Utc};

    #[test]
    fn each_temporal_type_binds_as_a_placeholder() {
        let d = NaiveDate::from_ymd_opt(2026, 7, 30).unwrap();
        let (sql, args) = bind_one("created_at", d);
        assert_eq!(sql, r#"("created_at" = $1)"#);
        assert_eq!(args, serde_json::json!(["2026-07-30"]));

        let t = d.and_hms_opt(12, 34, 56).unwrap();
        let (sql, args) = bind_one("created_at", t);
        assert_eq!(sql, r#"("created_at" = $1)"#);
        assert_eq!(args, serde_json::json!(["2026-07-30T12:34:56"]));

        let (sql, args) = bind_one("created_at", t.time());
        assert_eq!(sql, r#"("created_at" = $1)"#);
        assert_eq!(args, serde_json::json!(["12:34:56"]));

        let utc = Utc.with_ymd_and_hms(2026, 7, 30, 12, 34, 56).unwrap();
        let (sql, args) = bind_one("created_at", utc);
        assert_eq!(sql, r#"("created_at" = $1)"#);
        assert_eq!(args, serde_json::json!(["2026-07-30T12:34:56Z"]));
    }

    #[test]
    fn an_offset_datetime_binds_as_the_utc_instant() {
        // 21:34:56+09:00 and 12:34:56Z are the same instant; the offset is
        // gone by the time the argument list exists.
        let jst: DateTime<FixedOffset> = "2026-07-30T21:34:56+09:00".parse().unwrap();
        let (_, args) = bind_one("created_at", jst);
        assert_eq!(args, serde_json::json!(["2026-07-30T12:34:56Z"]));
    }
}

#[cfg(feature = "uuid")]
mod uuid_binds {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn a_uuid_binds_as_a_placeholder_and_serialises_hyphenated() {
        let u = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let (sql, args) = bind_one("id", u);
        assert_eq!(sql, r#"("id" = $1)"#);
        assert_eq!(
            args,
            serde_json::json!(["550e8400-e29b-41d4-a716-446655440000"])
        );
    }
}

#[cfg(feature = "decimal")]
mod decimal_binds {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn a_decimal_binds_as_a_placeholder_and_serialises_as_a_string() {
        // 19.99, scale 2 — the string keeps the scale a JSON number would lose.
        let (sql, args) = bind_one("price", Decimal::new(1999, 2));
        assert_eq!(sql, r#"("price" = $1)"#);
        assert_eq!(args, serde_json::json!(["19.99"]));
    }
}

#[cfg(feature = "json")]
mod json_binds {
    use super::*;

    #[test]
    fn a_json_document_binds_as_a_placeholder_and_serialises_structurally() {
        let doc = serde_json::json!({"tags": ["a", "b"], "n": 1});
        let (sql, args) = bind_one("meta", doc.clone());
        assert_eq!(sql, r#"("meta" = $1)"#);
        // The argument list carries the document itself, not a string of it.
        assert_eq!(args, serde_json::json!([doc]));
    }
}

/// `Option<T>` composes with the mapped types the same way it does with the
/// primitives: `None` is `NULL`.
#[cfg(feature = "uuid")]
#[test]
fn option_of_a_mapped_type_binds_null() {
    let (sql, args) = bind_one("id", None::<uuid::Uuid>);
    assert_eq!(sql, r#"("id" = $1)"#);
    assert_eq!(args, serde_json::json!([null]));
}
