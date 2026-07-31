use keelson_core::{FromValue, ToValue};

/// What a column's Rust type must be able to do: bind in
/// ([`ToValue`]) and read back out ([`FromValue`]).
///
/// This is the contract a code-generation column override hangs on. Blanket
/// over core's pair — deliberately not a third trait vocabulary: `ToValue` /
/// `FromValue` are already the two halves, `docs/type-mappings.md` already
/// defines their semantics per type, and the contract is defined against
/// [`Value`](keelson_core::Value) rather than any driver — so an override
/// type binds on every backend or on none.
///
/// Generated code names this bound explicitly per overridden column type —
/// `const _: () = keelson_exec::assert_bind::<UserId>();` — so a non-binding
/// override fails to compile in one line naming the type, not in an
/// inference swamp.
///
/// The message that line produces is keelson's own, not the compiler's default
/// walk through the blanket impl: `do_not_recommend` stops rustc from
/// re-reporting the failure as two unsatisfied supertrait bounds, and
/// `on_unimplemented` says the useful thing instead. That is worth an attribute
/// on two counts — the default said `ToValue is not implemented` twice, with a
/// list of unrelated types that happened to implement it, and the list's
/// contents drift with the compiler version (which is a failing UI test on the
/// next release, for no change in this crate).
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be a keelson column type",
    label = "not a column type",
    note = "a column type binds in and reads back out: it must implement both `ToValue` and `FromValue`",
    note = "for a newtype over one that already binds, `#[derive(Bind)]` (feature `macros`) or `bind_newtype!(Name(inner))` writes both impls"
)]
pub trait Bind: ToValue + FromValue + Send + 'static {}

#[diagnostic::do_not_recommend]
impl<T: ToValue + FromValue + Send + 'static> Bind for T {}

/// Assert at compile time that `T` can bind as a column.
pub const fn assert_bind<T: Bind>() {}

/// Implement [`ToValue`] and [`FromValue`] for a single-field newtype by
/// delegating to the inner type — the "derivable for newtypes" story, without
/// a proc-macro crate.
///
/// ```
/// use keelson_core::{FromValue, ToValue, Value};
///
/// #[derive(Debug, Clone, PartialEq)]
/// pub struct UserId(pub i64);
/// keelson_exec::bind_newtype!(UserId(i64));
///
/// const _: () = keelson_exec::assert_bind::<UserId>();
/// assert_eq!(UserId(7).to_value(), Value::I64(7));
/// assert_eq!(UserId::from_value(Value::I64(7)).unwrap(), UserId(7));
/// ```
///
/// Single-field tuple structs only: a multi-field type has no obvious single
/// column shape, and refusing is the honest move — write the two impls by
/// hand (about six lines) and put the domain rule where it belongs.
#[macro_export]
macro_rules! bind_newtype {
    ($name:ident($inner:ty)) => {
        impl $crate::__core::ToValue for $name {
            fn to_value(self) -> $crate::__core::Value {
                <$inner as $crate::__core::ToValue>::to_value(self.0)
            }
        }

        impl $crate::__core::FromValue for $name {
            fn from_value(
                v: $crate::__core::Value,
            ) -> ::std::result::Result<Self, $crate::__core::Error> {
                <$inner as $crate::__core::FromValue>::from_value(v).map($name)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use keelson_core::{FromValue, ToValue, Value};

    use super::assert_bind;

    #[derive(Debug, Clone, PartialEq)]
    struct UserId(i64);
    crate::bind_newtype!(UserId(i64));

    // The compile-error guarantee, exercised in the affirmative: this line is
    // what generated code will emit per overridden column type.
    const _: () = assert_bind::<UserId>();

    #[test]
    fn a_newtype_delegates_both_ways() {
        assert_eq!(UserId(7).to_value(), Value::I64(7));
        assert_eq!(UserId::from_value(Value::I64(7)).unwrap(), UserId(7));
        assert_eq!(UserId::from_value(Value::I32(7)).unwrap(), UserId(7)); // widening survives
        assert!(UserId::from_value(Value::Text("x".into())).is_err());
    }

    #[test]
    fn options_of_newtypes_still_bind() {
        // Option<T: Bind> composes through core's blanket impls.
        assert_eq!(Some(UserId(1)).to_value(), Value::I64(1));
        assert_eq!(Option::<UserId>::from_value(Value::Null).unwrap(), None);
    }
}
