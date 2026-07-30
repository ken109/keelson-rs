use std::fmt;
use std::sync::Arc;

use crate::error::Result;

/// Something that modifies a query in place.
///
/// Mods are the whole composition story: `psql::select((a, b, c))` is one mod made
/// of three, applied left to right. The query type is the type parameter, so a
/// mod for a statement that has no such clause simply does not implement
/// `Mod<ThatQuery>` and the invalid combination fails to compile.
///
/// `apply` consumes `self`, so a mod can hand owned data straight to the query
/// without cloning.
pub trait Mod<Q> {
    /// Apply this modification to `q`.
    fn apply(self, q: &mut Q);
}

/// No mods at all — `psql::select(())`.
impl<Q> Mod<Q> for () {
    fn apply(self, _q: &mut Q) {}
}

/// `None` applies nothing, which is how a conditional mod is written:
/// `cond.then(|| select::where_(..))`. No `if` statement, no `Vec` juggling.
impl<Q, M: Mod<Q>> Mod<Q> for Option<M> {
    fn apply(self, q: &mut Q) {
        if let Some(m) = self {
            m.apply(q);
        }
    }
}

impl<Q, M: Mod<Q>> Mod<Q> for Vec<M> {
    fn apply(self, q: &mut Q) {
        for m in self {
            m.apply(q);
        }
    }
}

impl<Q, M: Mod<Q>, const N: usize> Mod<Q> for [M; N] {
    fn apply(self, q: &mut Q) {
        for m in self {
            m.apply(q);
        }
    }
}

macro_rules! impl_mod_tuple {
    ($($name:ident),+) => {
        #[allow(non_snake_case)]
        impl<Q, $($name: Mod<Q>),+> Mod<Q> for ($($name,)+) {
            fn apply(self, q: &mut Q) {
                let ($($name,)+) = self;
                $($name.apply(q);)+
            }
        }
    };
}

impl_mod_tuple!(A);
impl_mod_tuple!(A, B);
impl_mod_tuple!(A, B, C);
impl_mod_tuple!(A, B, C, D);
impl_mod_tuple!(A, B, C, D, E);
impl_mod_tuple!(A, B, C, D, E, F);
impl_mod_tuple!(A, B, C, D, E, F, G);
impl_mod_tuple!(A, B, C, D, E, F, G, H);
impl_mod_tuple!(A, B, C, D, E, F, G, H, I);
impl_mod_tuple!(A, B, C, D, E, F, G, H, I, J);
impl_mod_tuple!(A, B, C, D, E, F, G, H, I, J, K);
impl_mod_tuple!(A, B, C, D, E, F, G, H, I, J, K, L);
impl_mod_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M);
impl_mod_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N);
impl_mod_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O);
impl_mod_tuple!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P);

/// A [`Mod`] from a closure.
///
/// The building block every `select::*` / `insert::*` helper is written in terms
/// of: `pub fn where_<Q: HasWhere>(e: impl Expression + 'static) -> impl Mod<Q>`
/// returns one of these.
pub struct ModFn<F>(F);

/// Wrap a closure as a [`Mod`].
///
/// Intentionally unbounded here: the query type comes from the [`Mod`] impl at
/// the use site, so an inline `|q: &mut SelectQuery|` needs no turbofish.
pub fn mod_fn<F>(f: F) -> ModFn<F> {
    ModFn(f)
}

impl<F> fmt::Debug for ModFn<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ModFn")
    }
}

impl<Q, F: FnOnce(&mut Q)> Mod<Q> for ModFn<F> {
    fn apply(self, q: &mut Q) {
        (self.0)(q);
    }
}

/// A mod that runs when the query is built rather than when it is assembled.
///
/// bob calls these contextual mods. They exist for what cannot be decided at
/// assembly time — a `WHERE` that depends on the schema in use, say — and they
/// run on every build, so a query keeps them as `Vec<Arc<dyn BuildMod<Q>>>` and
/// applies them to a clone of itself at the top of `write_sql`.
///
/// `&self` rather than `self`, because unlike a [`Mod`] they are applied more than
/// once. Unlike rendering they can fail, and they run before there is any SQL to
/// attach a failure to, so this one returns a `Result`; the caller records it on
/// the writer.
pub trait BuildMod<Q>: fmt::Debug + Send + Sync {
    /// Apply this modification to `q`, or explain why it cannot be applied.
    fn apply(&self, q: &mut Q) -> Result<()>;
}

impl<Q, T: BuildMod<Q> + ?Sized> BuildMod<Q> for Arc<T> {
    fn apply(&self, q: &mut Q) -> Result<()> {
        (**self).apply(q)
    }
}

impl<Q, T: BuildMod<Q> + ?Sized> BuildMod<Q> for Box<T> {
    fn apply(&self, q: &mut Q) -> Result<()> {
        (**self).apply(q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    /// Stand-in query: mods append their marker, so the applied order is visible.
    type Q = Vec<&'static str>;

    struct Push(&'static str);

    impl Mod<Q> for Push {
        fn apply(self, q: &mut Q) {
            q.push(self.0);
        }
    }

    fn applied<M: Mod<Q>>(m: M) -> Q {
        let mut q = Q::new();
        m.apply(&mut q);
        q
    }

    #[test]
    fn unit_applies_nothing() {
        assert_eq!(applied(()), Vec::<&str>::new());
    }

    #[test]
    fn tuples_apply_left_to_right() {
        assert_eq!(applied((Push("a"),)), vec!["a"]);
        assert_eq!(applied((Push("a"), Push("b"))), vec!["a", "b"]);
        assert_eq!(
            applied((Push("a"), Push("b"), Push("c"), Push("d"))),
            vec!["a", "b", "c", "d"]
        );
    }

    #[test]
    fn tuples_reach_arity_sixteen() {
        let q = applied((
            Push("1"),
            Push("2"),
            Push("3"),
            Push("4"),
            Push("5"),
            Push("6"),
            Push("7"),
            Push("8"),
            Push("9"),
            Push("10"),
            Push("11"),
            Push("12"),
            Push("13"),
            Push("14"),
            Push("15"),
            Push("16"),
        ));
        assert_eq!(q.len(), 16);
        assert_eq!(q.first(), Some(&"1"));
        assert_eq!(q.last(), Some(&"16"));
    }

    #[test]
    fn tuples_nest_so_arity_is_never_a_ceiling() {
        assert_eq!(
            applied((Push("a"), (Push("b"), (Push("c"), Push("d"))), Push("e"))),
            vec!["a", "b", "c", "d", "e"]
        );
    }

    #[test]
    fn tuples_mix_mod_kinds() {
        let q = applied((
            Push("first"),
            None::<Push>,
            Some(Push("maybe")),
            vec![Push("v1"), Push("v2")],
            [Push("a1"), Push("a2")],
            (),
            mod_fn(|q: &mut Q| q.push("closure")),
        ));
        assert_eq!(q, vec!["first", "maybe", "v1", "v2", "a1", "a2", "closure"]);
    }

    // `then` not `then_some`: the point is the idiom the design doc documents, and
    // a real mod is a function call that must stay unevaluated.
    #[allow(clippy::unnecessary_lazy_evaluations)]
    #[test]
    fn option_is_how_conditionals_are_written() {
        let admin = false;
        assert_eq!(applied((!admin).then(|| Push("scoped"))), vec!["scoped"]);
        let admin = true;
        assert_eq!(
            applied((!admin).then(|| Push("scoped"))),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn nested_options_collapse() {
        assert_eq!(applied(Some(Some(Push("deep")))), vec!["deep"]);
        assert_eq!(applied(Some(None::<Push>)), Vec::<&str>::new());
    }

    #[test]
    fn empty_collections_apply_nothing() {
        assert_eq!(applied(Vec::<Push>::new()), Vec::<&str>::new());
        assert_eq!(applied([] as [Push; 0]), Vec::<&str>::new());
    }

    /// An erased mod, as a list assembled at run time would hold them.
    type BoxedMod = Box<dyn FnOnce(&mut Q)>;

    #[test]
    fn a_vec_of_erased_mods_is_a_mod() {
        // The `Vec<M>` impl covers boxed mods too, which is what a runtime-built
        // list of conditions needs.
        let mods: Vec<BoxedMod> = vec![
            Box::new(|q: &mut Q| q.push("one")),
            Box::new(|q: &mut Q| q.push("two")),
        ];
        let mods: Vec<_> = mods.into_iter().map(mod_fn).collect();
        assert_eq!(applied(mods), vec!["one", "two"]);
    }

    #[derive(Debug)]
    struct AppendAtBuild(&'static str);

    impl BuildMod<Q> for AppendAtBuild {
        fn apply(&self, q: &mut Q) -> Result<()> {
            q.push(self.0);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct Refuse;

    impl BuildMod<Q> for Refuse {
        fn apply(&self, _q: &mut Q) -> Result<()> {
            Err(Error::Incomplete("a schema"))
        }
    }

    #[test]
    fn build_mods_run_repeatedly_and_can_fail() {
        let mods: Vec<Arc<dyn BuildMod<Q>>> = vec![Arc::new(AppendAtBuild("once"))];

        let mut first = Q::new();
        let mut second = Q::new();
        for m in &mods {
            m.apply(&mut first).unwrap();
            m.apply(&mut second).unwrap();
        }
        assert_eq!(first, vec!["once"]);
        assert_eq!(
            second,
            vec!["once"],
            "a build mod is not consumed by a build"
        );

        assert!(Refuse.apply(&mut first).is_err());
    }
}
