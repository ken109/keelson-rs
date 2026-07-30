//! `#[derive(Bind)]`: what the generated `ToValue`/`FromValue` pair does.
//!
//! Reached the way a user reaches it — `use keelson_core::Bind` behind the
//! `macros` feature — so the re-export path is part of what is tested.

use keelson_core::{Bind, Error, FromValue, ToValue, Value};

#[derive(Debug, Clone, PartialEq, Bind)]
struct UserId(i64);

#[derive(Debug, Clone, PartialEq, Bind)]
pub struct Email(pub String);

/// A named single field is the same newtype spelled differently.
#[derive(Debug, Clone, PartialEq, Bind)]
struct Slug {
    raw: String,
}

/// Generic newtypes carry the inner bound into the generated impls.
#[derive(Debug, Clone, PartialEq, Bind)]
struct Tagged<T>(T);

/// The mapped types are inner types like any other.
#[derive(Debug, Clone, PartialEq, Bind)]
struct Token(uuid::Uuid);

/// The compile-time half of the promise: this is the line keelson-gen emits
/// for every `[[types.override]]`, and a derived newtype satisfies it.
const _: () = keelson_exec::assert_bind::<UserId>();
const _: () = keelson_exec::assert_bind::<Email>();
const _: () = keelson_exec::assert_bind::<Slug>();
const _: () = keelson_exec::assert_bind::<Token>();
const _: () = keelson_exec::assert_bind::<Tagged<i64>>();

#[test]
fn a_tuple_newtype_delegates_both_ways() {
    assert_eq!(UserId(7).to_value(), Value::I64(7));
    assert_eq!(UserId::from_value(Value::I64(7)).unwrap(), UserId(7));
}

#[test]
fn a_named_single_field_struct_is_a_newtype_too() {
    assert_eq!(
        Slug { raw: "ada".into() }.to_value(),
        Value::Text("ada".into())
    );
    assert_eq!(
        Slug::from_value(Value::Text("ada".into())).unwrap(),
        Slug { raw: "ada".into() }
    );
}

#[test]
fn generic_newtypes_bind_at_every_inner_type() {
    assert_eq!(Tagged(1i64).to_value(), Value::I64(1));
    assert_eq!(
        Tagged("x".to_owned()).to_value(),
        Value::Text("x".to_owned())
    );
    assert_eq!(
        Tagged::<i64>::from_value(Value::I64(1)).unwrap(),
        Tagged(1i64)
    );
}

#[test]
fn a_mapped_type_inside_a_newtype_keeps_its_variant() {
    let u = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    assert_eq!(Token(u).to_value(), Value::Uuid(u));
    assert_eq!(Token::from_value(Value::Uuid(u)).unwrap(), Token(u));
}

/// Everything the inner type accepts, the newtype accepts: the widening a
/// driver forces (an `INTEGER` column handed back as `I32`) still works.
#[test]
fn the_inner_types_from_value_semantics_are_inherited() {
    assert_eq!(UserId::from_value(Value::I32(7)).unwrap(), UserId(7));
    assert_eq!(UserId::from_value(Value::U8(7)).unwrap(), UserId(7));
}

/// And so are its failures, unchanged — the derive adds no error layer of its
/// own, so the message a user sees is the inner type's.
#[test]
fn a_failure_is_the_inner_types_failure() {
    let e = UserId::from_value(Value::Text("x".into())).unwrap_err();
    assert!(
        matches!(
            e,
            Error::TypeMismatch {
                expected: "i64",
                found: "text"
            }
        ),
        "{e}"
    );
    assert_eq!(e.to_string(), "cannot read text as i64");

    let e = UserId::from_value(Value::Null).unwrap_err();
    assert_eq!(e.to_string(), "cannot read NULL as i64");
}

/// `Option<Newtype>` composes through core's blanket impls — a nullable
/// overridden column needs nothing extra.
#[test]
fn options_of_newtypes_bind_and_read() {
    assert_eq!(Some(UserId(1)).to_value(), Value::I64(1));
    assert_eq!(None::<UserId>.to_value(), Value::Null);
    assert_eq!(Option::<UserId>::from_value(Value::Null).unwrap(), None);
    assert_eq!(
        Option::<UserId>::from_value(Value::I64(1)).unwrap(),
        Some(UserId(1))
    );
}

/// A newtype over a newtype: nothing special, it is just another inner type.
#[test]
fn newtypes_nest() {
    #[derive(Debug, Clone, PartialEq, Bind)]
    struct AdminId(UserId);

    assert_eq!(AdminId(UserId(3)).to_value(), Value::I64(3));
    assert_eq!(
        AdminId::from_value(Value::I64(3)).unwrap(),
        AdminId(UserId(3))
    );
}

/// The derive does not require the inner field to be public, and does not make
/// it so: it expands where the type is defined.
#[test]
fn a_private_inner_field_is_reachable_only_to_the_derive() {
    mod private {
        use keelson_core::Bind;

        #[derive(Debug, Clone, PartialEq, Bind)]
        pub(crate) struct Secret(String);

        pub(crate) fn make(s: &str) -> Secret {
            Secret(s.to_owned())
        }
    }

    assert_eq!(private::make("s").to_value(), Value::Text("s".into()));
}

/// Bytes, bools and floats — the derive is type-agnostic, so one case each is
/// enough to show it is not integer-shaped.
#[test]
fn any_bindable_inner_type_works() {
    #[derive(Debug, Clone, PartialEq, Bind)]
    struct Blob(Vec<u8>);
    #[derive(Debug, Clone, PartialEq, Bind)]
    struct Flag(bool);
    #[derive(Debug, Clone, PartialEq, Bind)]
    struct Ratio(f64);

    assert_eq!(Blob(vec![1, 2]).to_value(), Value::Bytes(vec![1, 2]));
    assert_eq!(Flag(true).to_value(), Value::Bool(true));
    assert_eq!(Ratio(1.5).to_value(), Value::F64(1.5));
    assert_eq!(
        Blob::from_value(Value::Bytes(vec![1])).unwrap(),
        Blob(vec![1])
    );
}

/// A newtype over `Option<T>` is a nullable column in one type, which the
/// delegation gives for free.
#[test]
fn a_newtype_over_an_option_is_nullable() {
    #[derive(Debug, Clone, PartialEq, Bind)]
    struct Nickname(Option<String>);

    assert_eq!(Nickname(None).to_value(), Value::Null);
    assert_eq!(Nickname::from_value(Value::Null).unwrap(), Nickname(None));
}
