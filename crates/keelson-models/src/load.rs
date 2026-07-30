//! The attachment step of a then-load: children came back from the second
//! query, and each parent's `rel` field wants its own.
//!
//! Generated then-load closures call one of these two functions; they are the
//! only interesting lines a then-loader has, so they live here rather than
//! being emitted over and over.

use std::collections::HashMap;
use std::hash::Hash;

/// Attach a to-one relation: each parent gets the child whose key matches, or
/// `None`. Children are cloned only where several parents share one.
pub fn attach_to_one<P, C, K>(
    parents: &mut [P],
    children: Vec<C>,
    parent_key: impl Fn(&P) -> K,
    child_key: impl Fn(&C) -> K,
    mut attach: impl FnMut(&mut P, Option<C>),
) where
    K: Eq + Hash,
    C: Clone,
{
    let by_key: HashMap<K, C> = children.into_iter().map(|c| (child_key(&c), c)).collect();
    for p in parents {
        let child = by_key.get(&parent_key(p)).cloned();
        attach(p, child);
    }
}

/// Attach a to-many relation: each parent gets every child whose key matches.
///
/// Each child is attached exactly once — parents are assumed key-distinct,
/// which they are when the key is the parent's primary key (the shape every
/// generated then-load has).
pub fn attach_to_many<P, C, K>(
    parents: &mut [P],
    children: Vec<C>,
    parent_key: impl Fn(&P) -> K,
    child_key: impl Fn(&C) -> K,
    mut attach: impl FnMut(&mut P, Vec<C>),
) where
    K: Eq + Hash,
{
    let mut by_key: HashMap<K, Vec<C>> = HashMap::new();
    for c in children {
        by_key.entry(child_key(&c)).or_default().push(c);
    }
    for p in parents {
        let own = by_key.remove(&parent_key(p)).unwrap_or_default();
        attach(p, own);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Parent {
        id: i32,
        children: Vec<i32>,
        one: Option<i32>,
    }

    fn parents() -> Vec<Parent> {
        (1..=3)
            .map(|id| Parent {
                id,
                children: Vec::new(),
                one: None,
            })
            .collect()
    }

    #[test]
    fn to_many_groups_by_key_and_leaves_misses_empty() {
        let mut ps = parents();
        // (child id, parent id): parent 1 has two, parent 3 none.
        let children = vec![(10, 1), (11, 1), (20, 2)];
        attach_to_many(
            &mut ps,
            children,
            |p| p.id,
            |c| c.1,
            |p, cs| p.children = cs.into_iter().map(|c| c.0).collect(),
        );
        assert_eq!(ps[0].children, vec![10, 11]);
        assert_eq!(ps[1].children, vec![20]);
        assert_eq!(ps[2].children, Vec::<i32>::new());
    }

    #[test]
    fn to_one_attaches_a_match_or_none_and_shares_children() {
        let mut ps = parents();
        ps[1].id = 1; // two parents share the same key
        let children = vec![(100, 1)];
        attach_to_one(
            &mut ps,
            children,
            |p| p.id,
            |c| c.1,
            |p, c| p.one = c.map(|c| c.0),
        );
        assert_eq!(ps[0].one, Some(100));
        assert_eq!(ps[1].one, Some(100), "a shared child is cloned, not stolen");
        assert_eq!(ps[2].one, None);
    }
}
