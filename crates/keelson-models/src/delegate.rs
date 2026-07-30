//! Clause delegation: the model wrappers implement keelson-core's `Has*`
//! traits by forwarding to the dialect statement they hold.
//!
//! This is what makes "Layer 1 mods mix in directly" true: every shared
//! dialect mod is generic over a `Has*` trait (`select::limit<Q: HasLimit>`),
//! so with these impls in place the mod instantiates at the *wrapper* — no
//! conversion, no `.into_inner()`, the wrapper simply is one more statement
//! type the mod applies to. A mod written against the concrete dialect struct
//! (the rare, statement-specific ones like psql's `select::distinct()`) goes
//! through the wrapper's `apply` escape hatch instead.

/// One `Has*` delegation impl: `$wrapper<M>` implements the trait whenever
/// the associated statement type does.
macro_rules! delegate_clause {
    ($wrapper:ident, $bound:ident, $assoc:ident, $trait_:ident, $method:ident, $ret:ty) => {
        impl<M: $crate::$bound> ::keelson_core::clause::$trait_ for $wrapper<M>
        where
            M::$assoc: ::keelson_core::clause::$trait_,
        {
            fn $method(&mut self) -> &mut $ret {
                ::keelson_core::clause::$trait_::$method(&mut self.query)
            }
        }
    };
}

pub(crate) use delegate_clause;
