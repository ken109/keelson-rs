use keelson_core::expr::Expr;
use keelson_core::{ToValue, Value};

/// One field of a generated `Setter`: unset, `NULL`, or a value — three states,
/// distinguished by type.
///
/// The three states are the whole point (bob's `omit`/`null`/`value`): an
/// **unset** field does not appear in the `INSERT` or `UPDATE` at all — the
/// column keeps its database default on insert and its current value on update
/// — while **`Null`** writes SQL `NULL` explicitly. Collapsing the first two
/// into `Option` would make "leave it alone" and "erase it" the same call
/// site, which is precisely the bug this type exists to prevent.
///
/// `Default` is [`Unset`](Set::Unset), which is what makes the struct-update
/// spelling work:
///
/// ```ignore
/// users::table().insert(users::Setter {
///     name: set("Stephen"),
///     email: null(),
///     ..Default::default()      // every other column: untouched
/// })
/// ```
///
/// `Null` is representable for every column, `NOT NULL` ones included — the
/// constraint violation is the database's to report, exactly as it is for raw
/// SQL. Encoding nullability in the `Setter`'s types was considered and
/// rejected: it would split `Set<T>` into two vocabularies and make every
/// generated field's type depend on a constraint the schema can change,
/// for a check the engine performs anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Set<T> {
    /// Not mentioned: the field contributes nothing to the statement.
    #[default]
    Unset,
    /// Explicitly `NULL`, bound as an argument.
    Null,
    /// A value, bound as an argument.
    Value(T),
}

/// A set field: `set("Stephen")` is `Set::Value("Stephen".into())`.
///
/// Takes `impl Into<T>` so the ergonomic literals work — `set("Stephen")` for
/// a `String` column — while the target type still comes from the `Setter`
/// field, so `set` stays as typed as the column it lands in.
pub fn set<T>(value: impl Into<T>) -> Set<T> {
    Set::Value(value.into())
}

/// An explicit SQL `NULL` — [`Set::Null`], spelled the way the sketch spells
/// it.
pub fn null<T>() -> Set<T> {
    Set::Null
}

impl<T> Set<T> {
    /// Whether the field is [`Unset`](Set::Unset) and so contributes nothing.
    pub fn is_unset(&self) -> bool {
        matches!(self, Set::Unset)
    }
}

impl<T: ToValue> Set<T> {
    /// The bound expression this field contributes: `None` when unset, a bound
    /// `NULL` for [`Set::Null`], a bound argument for a value.
    ///
    /// Always a placeholder, never an inline literal — the same binding-only
    /// contract every mapped type follows (`docs/type-mappings.md`).
    pub fn into_expr(self) -> Option<Expr> {
        match self {
            Set::Unset => None,
            Set::Null => Some(Expr::Arg(Value::Null)),
            Set::Value(v) => Some(Expr::Arg(v.to_value())),
        }
    }

    /// Contribute this field to an `INSERT`'s column and value lists — one
    /// call per field is the shape generated `insert_query` bodies take.
    pub fn push_into(
        self,
        column: &'static str,
        columns: &mut Vec<&'static str>,
        values: &mut Vec<Expr>,
    ) {
        if let Some(e) = self.into_expr() {
            columns.push(column);
            values.push(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unset_so_struct_update_syntax_leaves_fields_out() {
        #[derive(Default)]
        struct Setter {
            name: Set<String>,
            email: Set<String>,
            age: Set<i32>,
        }

        let s = Setter {
            name: set("Stephen"),
            email: null(),
            ..Default::default()
        };
        assert_eq!(s.name, Set::Value("Stephen".to_owned()));
        assert_eq!(s.email, Set::Null);
        assert_eq!(s.age, Set::Unset);
        assert!(s.age.is_unset());
    }

    #[test]
    fn the_three_states_produce_nothing_null_and_a_bound_value() {
        assert!(Set::<i32>::Unset.into_expr().is_none());
        assert!(matches!(
            null::<i32>().into_expr(),
            Some(Expr::Arg(Value::Null))
        ));
        assert!(matches!(
            set::<i32>(7).into_expr(),
            Some(Expr::Arg(Value::I32(7)))
        ));
    }

    #[test]
    fn push_into_skips_unset_fields_entirely() {
        let mut cols = Vec::new();
        let mut vals = Vec::new();
        set::<String>("ada").push_into("name", &mut cols, &mut vals);
        Set::<String>::Unset.push_into("email", &mut cols, &mut vals);
        null::<i32>().push_into("age", &mut cols, &mut vals);
        assert_eq!(cols, vec!["name", "age"]);
        assert_eq!(vals.len(), 2, "the unset column binds nothing at all");
    }
}
