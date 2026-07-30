//! `#[derive(FromRow)]`: the mapping, the two field options, and the error
//! behaviour the generated body inherits from `Row::take`.
//!
//! No database here — a `Row` is owned, driver-free and constructible, which
//! is the property that makes row mapping testable at all (see
//! `docs/execution.md`). The end-to-end proof against a real engine is
//! `tests/end_to_end.rs`.

use std::sync::Arc;

use keelson_core::{Bind, FromRow, Value};
use keelson_exec::{Column, ExecError, FromRow as _, Row};

fn row(pairs: &[(&str, Value)]) -> Row {
    let columns: Arc<[Column]> = pairs
        .iter()
        .map(|(n, _)| Column::new(*n))
        .collect::<Vec<_>>()
        .into();
    Row::new(columns, pairs.iter().map(|(_, v)| v.clone()).collect())
}

#[derive(Debug, PartialEq, FromRow)]
struct Account {
    id: i64,
    #[keelson(rename = "email_address")]
    email: String,
    nickname: Option<String>,
}

#[test]
fn fields_map_to_columns_by_name_and_rename_overrides() {
    let mut r = row(&[
        ("id", Value::I64(1)),
        ("email_address", Value::Text("ada@example.com".into())),
        ("nickname", Value::Null),
    ]);
    assert_eq!(
        Account::from_row(&mut r).unwrap(),
        Account {
            id: 1,
            email: "ada@example.com".into(),
            nickname: None,
        }
    );
}

/// By name, not by position: the same columns in any order map the same.
#[test]
fn column_order_does_not_matter() {
    let mut r = row(&[
        ("nickname", Value::Text("countess".into())),
        ("email_address", Value::Text("ada@example.com".into())),
        ("id", Value::I64(1)),
    ]);
    assert_eq!(
        Account::from_row(&mut r).unwrap(),
        Account {
            id: 1,
            email: "ada@example.com".into(),
            nickname: Some("countess".into()),
        }
    );
}

/// Extra columns — `SELECT *` against a table that grew a column — are
/// ignored, not an error.
#[test]
fn unmapped_columns_are_left_alone() {
    let mut r = row(&[
        ("id", Value::I64(1)),
        ("email_address", Value::Text("a@b".into())),
        ("nickname", Value::Null),
        ("created_at", Value::Text("2026-07-30".into())),
    ]);
    assert!(Account::from_row(&mut r).is_ok());
}

/// The generated body uses `take`, so a mapped `String` moves out of the row
/// rather than being cloned. The observable consequence: what is left behind
/// is NULL.
#[test]
fn mapping_consumes_the_columns_it_reads() {
    let mut r = row(&[
        ("id", Value::I64(1)),
        ("email_address", Value::Text("a@b".into())),
        ("nickname", Value::Null),
    ]);
    Account::from_row(&mut r).unwrap();
    assert_eq!(r.value("email_address"), Some(&Value::Null));
}

#[test]
fn a_missing_column_names_it_and_lists_what_was_there() {
    let mut r = row(&[("id", Value::I64(1)), ("nickname", Value::Null)]);
    let e = Account::from_row(&mut r).unwrap_err();
    assert!(matches!(e, ExecError::MissingColumn { .. }), "{e}");
    assert_eq!(
        e.to_string(),
        "no column \"email_address\" in result set (columns: id, nickname)"
    );
}

/// The renamed column, not the field, is what a decode failure names — the
/// database's vocabulary is the one a user can act on.
#[test]
fn a_decode_failure_names_the_column_as_the_database_spells_it() {
    let mut r = row(&[
        ("id", Value::I64(1)),
        ("email_address", Value::I64(9)),
        ("nickname", Value::Null),
    ]);
    let e = Account::from_row(&mut r).unwrap_err();
    assert_eq!(
        e.to_string(),
        "column \"email_address\": cannot read i64 as String"
    );
}

/// A NULL in a non-`Option` field is the same error, and says which column.
#[test]
fn null_into_a_non_option_field_names_the_column() {
    let mut r = row(&[
        ("id", Value::I64(1)),
        ("email_address", Value::Null),
        ("nickname", Value::Null),
    ]);
    let e = Account::from_row(&mut r).unwrap_err();
    assert_eq!(
        e.to_string(),
        "column \"email_address\": cannot read NULL as String"
    );
}

// ─────────────────────────────── flatten ───────────────────────────────

#[derive(Debug, PartialEq, FromRow)]
struct Audit {
    created_by: i64,
    #[keelson(rename = "created_at")]
    at: String,
}

#[derive(Debug, PartialEq, FromRow)]
struct Post {
    id: i64,
    #[keelson(flatten)]
    audit: Audit,
}

#[test]
fn flatten_reads_the_nested_struct_out_of_the_same_row() {
    let mut r = row(&[
        ("id", Value::I64(4)),
        ("created_by", Value::I64(9)),
        ("created_at", Value::Text("2026-07-30".into())),
    ]);
    assert_eq!(
        Post::from_row(&mut r).unwrap(),
        Post {
            id: 4,
            audit: Audit {
                created_by: 9,
                at: "2026-07-30".into(),
            },
        }
    );
}

/// A column missing from the *nested* struct still reports the column, not the
/// field path — the nested impl is the one doing the reading.
#[test]
fn a_flattened_struct_reports_its_own_missing_columns() {
    let mut r = row(&[("id", Value::I64(4)), ("created_by", Value::I64(9))]);
    let e = Post::from_row(&mut r).unwrap_err();
    assert_eq!(
        e.to_string(),
        "no column \"created_at\" in result set (columns: id, created_by)"
    );
}

/// Flatten nests arbitrarily deep, because it is just another `FromRow` call.
#[test]
fn flatten_nests() {
    #[derive(Debug, PartialEq, FromRow)]
    struct Inner {
        c: i64,
    }
    #[derive(Debug, PartialEq, FromRow)]
    struct Middle {
        b: i64,
        #[keelson(flatten)]
        inner: Inner,
    }
    #[derive(Debug, PartialEq, FromRow)]
    struct Outer {
        a: i64,
        #[keelson(flatten)]
        middle: Middle,
    }

    let mut r = row(&[
        ("a", Value::I64(1)),
        ("b", Value::I64(2)),
        ("c", Value::I64(3)),
    ]);
    assert_eq!(
        Outer::from_row(&mut r).unwrap(),
        Outer {
            a: 1,
            middle: Middle {
                b: 2,
                inner: Inner { c: 3 },
            },
        }
    );
}

// ────────────────────── newtypes, generics, keywords ──────────────────────

#[derive(Debug, Clone, PartialEq, Bind)]
struct UserId(i64);

/// The two derives meet here: a `Bind` newtype is a field type like any other,
/// which is exactly the code generation override story.
#[test]
fn a_derived_newtype_is_a_field_type() {
    #[derive(Debug, PartialEq, FromRow)]
    struct Membership {
        user_id: UserId,
        owner: Option<UserId>,
    }

    let mut r = row(&[("user_id", Value::I64(7)), ("owner", Value::Null)]);
    assert_eq!(
        Membership::from_row(&mut r).unwrap(),
        Membership {
            user_id: UserId(7),
            owner: None,
        }
    );
}

/// A generic mapper picks up bounds on its field types.
#[test]
fn generic_structs_map() {
    #[derive(Debug, PartialEq, FromRow)]
    struct Envelope<T> {
        id: i64,
        payload: T,
    }

    let mut r = row(&[("id", Value::I64(1)), ("payload", Value::Text("x".into()))]);
    assert_eq!(
        Envelope::<String>::from_row(&mut r).unwrap(),
        Envelope {
            id: 1,
            payload: "x".to_owned(),
        }
    );
}

/// A raw identifier field maps to the column it is named after, without the
/// `r#`.
#[test]
fn raw_identifiers_map_to_the_bare_column_name() {
    #[derive(Debug, PartialEq, FromRow)]
    struct Row_ {
        r#type: String,
    }

    let mut r = row(&[("type", Value::Text("post".into()))]);
    assert_eq!(
        Row_::from_row(&mut r).unwrap(),
        Row_ {
            r#type: "post".to_owned(),
        }
    );
}

/// Reading a raw `Value` back is legal — `Value: FromValue` is the identity —
/// so a mapper can keep a column undecoded.
#[test]
fn a_field_can_stay_a_value() {
    #[derive(Debug, PartialEq, FromRow)]
    struct Raw {
        payload: Value,
    }

    let mut r = row(&[("payload", Value::Bytes(vec![1, 2]))]);
    assert_eq!(
        Raw::from_row(&mut r).unwrap(),
        Raw {
            payload: Value::Bytes(vec![1, 2]),
        }
    );
}

/// The two derives on one struct: legal, as long as the field carries no
/// options (`#[derive(Bind)]` refuses the whole `keelson` namespace, so a
/// `rename` here would be a compile error — see `tests/compile_fail`).
#[test]
fn a_one_field_struct_can_derive_both() {
    #[derive(Debug, PartialEq, Bind, FromRow)]
    struct Count {
        n: i64,
    }

    let mut r = row(&[("n", Value::I64(3))]);
    assert_eq!(Count::from_row(&mut r).unwrap(), Count { n: 3 });
    assert_eq!(
        keelson_core::ToValue::to_value(Count { n: 3 }),
        Value::I64(3)
    );
}
