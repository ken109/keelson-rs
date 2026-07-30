use std::fmt;
use std::sync::Arc;

use keelson_models::Set;

use crate::Faker;

/// One column of a factory template: where the value comes from.
///
/// A template field holds a `Source<T>`, and mods rewrite it —
/// `fac::users::id(10)` sets [`Source::Value`], `fac::users::random_id()`
/// sets a [`Source::Gen`]. At build time [`resolve`](Source::resolve) turns
/// the source into the three-state [`Set`] the model's `Setter` wants, with
/// the template's own default rule filling [`Auto`](Source::Auto).
///
/// Why five states where `Set` has three: a template needs to distinguish
/// "let the template's default rule decide" ([`Auto`](Source::Auto)) from
/// "leave the column out of the statement" ([`Omit`](Source::Omit)) — in a
/// `Setter` those collapse into `Unset`, but in a factory the first means
/// "sequence/random/whatever the spec says" and the second means "the
/// database default, explicitly".
pub enum Source<T> {
    /// The template's default rule decides — a sequence value for a unique
    /// column, a random value for a data column, omission for a column whose
    /// database default is the point.
    Auto,
    /// Exactly this value.
    Value(T),
    /// SQL `NULL`, explicitly.
    Null,
    /// Leave the column out of the statement — the database default applies.
    Omit,
    /// A caller-supplied generator, drawing from the run's [`Faker`] — so a
    /// custom random source is still covered by the determinism switch.
    Gen(Arc<dyn Fn(&mut Faker) -> T + Send + Sync>),
}

impl<T> Source<T> {
    /// A generated value: `Source::from_fn(|f| f.i64_in(1, 1000))`.
    pub fn from_fn(g: impl Fn(&mut Faker) -> T + Send + Sync + 'static) -> Self {
        Source::Gen(Arc::new(g))
    }
}

impl<T: Clone> Source<T> {
    /// The [`Set`] this source contributes, with `auto` — the template's
    /// per-column default rule — deciding [`Auto`](Source::Auto). `auto`
    /// returns a `Set` rather than a `T` so a rule can itself be "omit"
    /// (a column whose database default is the right test value).
    pub fn resolve(&self, f: &mut Faker, auto: impl FnOnce(&mut Faker) -> Set<T>) -> Set<T> {
        match self {
            Source::Auto => auto(f),
            Source::Value(v) => Set::Value(v.clone()),
            Source::Null => Set::Null,
            Source::Omit => Set::Unset,
            Source::Gen(g) => Set::Value(g(f)),
        }
    }
}

// Manual, not `#[derive(Default)]`: the derive would bound `T: Default`,
// which `Auto` does not need.
#[allow(clippy::derivable_impls)]
impl<T> Default for Source<T> {
    fn default() -> Self {
        Source::Auto
    }
}

impl<T: Clone> Clone for Source<T> {
    fn clone(&self) -> Self {
        match self {
            Source::Auto => Source::Auto,
            Source::Value(v) => Source::Value(v.clone()),
            Source::Null => Source::Null,
            Source::Omit => Source::Omit,
            Source::Gen(g) => Source::Gen(Arc::clone(g)),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Source<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Auto => f.write_str("Auto"),
            Source::Value(v) => f.debug_tuple("Value").field(v).finish(),
            Source::Null => f.write_str("Null"),
            Source::Omit => f.write_str("Omit"),
            Source::Gen(_) => f.write_str("Gen(..)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn faker() -> Faker {
        Faker::seeded(0)
    }

    #[test]
    fn auto_defers_to_the_default_rule_in_both_directions() {
        let s = Source::<i64>::Auto;
        assert_eq!(s.resolve(&mut faker(), |_| Set::Value(7)), Set::Value(7));
        assert_eq!(s.resolve(&mut faker(), |_| Set::Unset), Set::Unset);
    }

    #[test]
    fn value_null_and_omit_map_onto_the_setter_states() {
        assert_eq!(
            Source::Value(3i64).resolve(&mut faker(), |_| Set::Unset),
            Set::Value(3)
        );
        assert_eq!(
            Source::<i64>::Null.resolve(&mut faker(), |_| Set::Value(1)),
            Set::Null
        );
        assert_eq!(
            Source::<i64>::Omit.resolve(&mut faker(), |_| Set::Value(1)),
            Set::Unset
        );
    }

    #[test]
    fn gen_draws_from_the_faker_so_the_seed_covers_it() {
        let s = Source::from_fn(|f: &mut Faker| f.i64_in(0, 1_000_000));
        let a = s.resolve(&mut Faker::seeded(9), |_| Set::Unset);
        let b = s.clone().resolve(&mut Faker::seeded(9), |_| Set::Unset);
        assert_eq!(a, b, "same seed, same generated value");
        assert!(matches!(a, Set::Value(_)));
    }

    #[test]
    fn debug_names_the_variant_without_requiring_the_closure_to() {
        assert_eq!(format!("{:?}", Source::<i64>::Auto), "Auto");
        assert_eq!(
            format!("{:?}", Source::from_fn(|f: &mut Faker| f.next_u64())),
            "Gen(..)"
        );
    }
}
