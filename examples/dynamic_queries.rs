//! **Composing a query at run time.** The filter that is only sometimes
//! applied, without string assembly.
//!
//!     cargo run -p keelson-examples --example dynamic_queries
//!
//! This is the thing a compile-time-checked macro cannot do and the reason
//! Layer 1 exists. Because a mod is a value:
//!
//! - `Option<M>` is a mod, and `None` contributes nothing;
//! - `Vec<M>` and `[M; N]` are mods, so a list of filters built in a loop is
//!   one mod;
//! - `()` is a mod, so "no options at all" needs no special case;
//! - a function can *return* `impl Mod<SelectQuery>`, so the house rules for
//!   pagination or soft deletes live in one place.
//!
//! Nothing here concatenates SQL, and every value still arrives as a bound
//! placeholder.

use keelson::prelude::*;
use keelson::sqlite::{self, Expr, SelectQuery, arg, args, quote, select};
use keelson_examples::show;

/// What a search form might hand the query layer: every field optional.
#[derive(Debug, Default)]
struct Search {
    name_contains: Option<String>,
    min_age: Option<i64>,
    active_only: bool,
    ids: Vec<i64>,
    page: Option<(i64, i64)>,
}

/// The house pagination rule, in one place, as a value.
fn paginate(page: Option<(i64, i64)>) -> impl Mod<SelectQuery> {
    // `Option<M>` is a mod: `None` applies nothing at all. No `if` needed at
    // the call site, and no "0 means unlimited" convention to remember.
    page.map(|(limit, offset)| (select::limit(arg(limit)), select::offset(arg(offset))))
}

/// Turn the form into a statement. Note the return type: an ordinary
/// `SelectQuery`, indistinguishable from a hand-written one.
fn search(s: &Search) -> SelectQuery {
    // The filters that are conditions can be collected as expressions and
    // handed over as one `where_`, or applied as separate `where_` mods --
    // several `where_`s are `AND`ed, so both spellings mean the same thing.
    // Collecting keeps the decision "is this filter on?" next to the filter.
    let mut conditions: Vec<Expr> = Vec::new();
    if let Some(pattern) = &s.name_contains {
        conditions.push(quote("name").like(arg(format!("%{pattern}%"))));
    }
    if let Some(age) = s.min_age {
        conditions.push(quote("age").gte(arg(age)));
    }
    if s.active_only {
        conditions.push(quote("is_active").eq(arg(true)));
    }
    if !s.ids.is_empty() {
        // `args` binds a whole list as a comma-separated group of
        // placeholders -- the right-hand side of an `IN`.
        conditions.push(quote("id").in_(args(s.ids.clone())));
    }

    sqlite::select((
        select::from(quote("users")),
        // `Vec<M>` is a mod. An empty one applies nothing, so "no filters"
        // needs no branch here either.
        conditions
            .into_iter()
            .map(select::where_)
            .collect::<Vec<_>>(),
        select::order_by(quote("id")),
        paginate(s.page),
    ))
}

fn main() -> keelson::Result<()> {
    // Nothing set: no WHERE, no LIMIT. The empty `Vec` and the `None`
    // contributed nothing, and the statement is still valid SQL.
    let (sql, args) = search(&Search::default()).build()?;
    show("no filters at all", &sql, &args);
    assert_eq!(sql, r#"SELECT * FROM "users" ORDER BY "id""#);
    assert!(args.is_empty());

    // One filter.
    let (sql, args) = search(&Search {
        min_age: Some(21),
        ..Default::default()
    })
    .build()?;
    show("one filter", &sql, &args);
    assert_eq!(
        sql,
        r#"SELECT * FROM "users" WHERE ("age" >= ?1) ORDER BY "id""#
    );

    // Everything at once. The placeholders are numbered in render order, not
    // in the order the mods were assembled -- the writer does that at build
    // time, which is why a mod can be moved around freely.
    let (sql, args) = search(&Search {
        name_contains: Some("ad".to_owned()),
        min_age: Some(21),
        active_only: true,
        ids: vec![1, 2, 3],
        page: Some((20, 40)),
    })
    .build()?;
    show("every filter", &sql, &args);
    assert_eq!(
        sql,
        concat!(
            r#"SELECT * FROM "users" WHERE ("name" LIKE ?1) AND ("age" >= ?2) "#,
            r#"AND ("is_active" = ?3) AND ("id" IN (?4, ?5, ?6)) "#,
            r#"ORDER BY "id" LIMIT ?7 OFFSET ?8"#
        )
    );
    assert_eq!(args.len(), 8);

    // ── growing a statement after the fact ──────────────────────────────
    //
    // `apply` puts a mod onto an already-built statement, which is what a
    // repository method does when it takes "extra conditions" from a caller.
    let mut q = sqlite::select((select::from(quote("posts")), select::limit(5)));
    let caller_supplied: Option<_> = Some(select::where_(quote("status").eq(arg("published"))));
    q.apply(caller_supplied);
    let (sql, _) = q.build()?;
    show("applied after the fact", &sql, &[]);
    assert!(sql.contains(r#"WHERE ("status" = ?1)"#));

    // ── one filter, several statements ──────────────────────────────────
    //
    // A mod is generic over the statement, so "only rows this user owns" is
    // written once and applies to the SELECT, the UPDATE and the DELETE.
    fn owned_by(user_id: i64) -> impl Mod<SelectQuery> {
        select::where_(quote("user_id").eq(arg(user_id)))
    }
    let (sql, _) = sqlite::select((select::from(quote("posts")), owned_by(7))).build()?;
    assert!(sql.contains(r#"WHERE ("user_id" = ?1)"#));

    println!("ok");
    Ok(())
}
