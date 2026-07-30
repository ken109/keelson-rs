/// A **required** parent reference — the template field behind a non-null
/// foreign key.
///
/// The default is [`Auto`](Parent::Auto): at create time the parent's own
/// default template is created first (recursively — a comment chains a post
/// chains a user) and the FK takes the created row's key. That is the
/// schema-aware promise: `create_many(&db, 10)` on the deepest table makes
/// the whole chain exist, ten times over, each row with its own parents
/// (FactoryBot's association semantics — share a parent by passing it in as
/// [`Existing`](Parent::Existing) instead).
///
/// `build()` (no database) fills the FK column only for `Existing`; `Auto`
/// and `Template` need a database to produce a key, so `build()` leaves the
/// column unset — recorded in the crate docs.
#[derive(Debug, Clone, PartialEq)]
pub enum Parent<T, Pk> {
    /// Create the parent from its default template.
    Auto,
    /// Create the parent from this shaped template — `for_post(…)`.
    Template(T),
    /// Use this already-existing row's key — `post(&p)` / `post_id(k)`.
    Existing(Pk),
}

// Manual, not `#[derive(Default)]`: the derive would bound `T: Default` and
// `Pk: Default`, which `Auto` does not need — and a template type without
// `Default` should still be usable as a `Parent`'s `T`.
#[allow(clippy::derivable_impls)]
impl<T, Pk> Default for Parent<T, Pk> {
    fn default() -> Self {
        Parent::Auto
    }
}

/// An **optional** parent reference — the template field behind a nullable
/// foreign key.
///
/// The default is [`Absent`](OptionalParent::Absent): the column stays NULL,
/// because a factory should never invent rows the schema does not require. A
/// mod opts in with an existing row or a shaped template.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionalParent<T, Pk> {
    /// No parent; the FK column stays NULL.
    Absent,
    /// Create the parent from this shaped template.
    Template(T),
    /// Use this already-existing row's key.
    Existing(Pk),
}

// Manual for the same no-spurious-bounds reason as `Parent`'s.
#[allow(clippy::derivable_impls)]
impl<T, Pk> Default for OptionalParent<T, Pk> {
    fn default() -> Self {
        OptionalParent::Absent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_policy_auto_create_required_skip_optional() {
        let p: Parent<(), i64> = Parent::default();
        assert_eq!(p, Parent::Auto);
        let o: OptionalParent<(), i64> = OptionalParent::default();
        assert_eq!(o, OptionalParent::Absent);
    }
}
