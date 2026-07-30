//! Combinatorial clause coverage: every optional clause crossed with every other.
//!
//! The per-clause grammar walks (`grammar_*.rs`) test each clause once, next to
//! nothing else. What they cannot catch is an *interaction*: a clause that
//! renders in the wrong order once another is present, or — the silent one — an
//! argument that binds to the wrong `?` because a clause that renders earlier
//! claimed its slot. A clause tested alone always binds first, so only a cross
//! product sees that. MySQL's placeholders are all spelled `?`, so unlike psql
//! and sqlite the numbering never shows in the SQL — the **argument vector's
//! order** is the whole of invariant 3 here, and every clause binds distinct
//! values so a swapped pair cannot cancel out.
//!
//! # Where the expectation comes from
//!
//! A hand-written expected string per combination is impossible, and pasting the
//! builder's output would make the suite assert that the code equals itself.
//! Instead each optional clause carries a **fragment derived from its production
//! in the MySQL 8.4 reference manual**, and the expected statement is those
//! fragments concatenated in the *manual's* clause order — which the builder
//! does not get to choose. `#` marks a bound placeholder.
//!
//! # What stands in for a parse-tree check
//!
//! psql's combinatorial tier can ask `pg_query` — PostgreSQL's own parser —
//! whether the tree contains exactly the clauses asked for. MySQL has no such
//! oracle: the only AST available is `sqlparser`'s, and that is the same crate
//! this workspace has already measured to be wrong in both directions (it
//! accepts PostgreSQL-only `DISTINCT ON`, rejects valid multi-table
//! `UPDATE a, b SET …`). Inspecting an unreliable parser's tree would launder
//! its mistakes into the expectation, so it is deliberately not used for shape.
//! The stand-in is stronger: **full-string equality against the
//! manual-derived assembly**, which pins clause presence, absence and order at
//! once, plus the argument-order check. `sqlparser` is still run as an
//! *advisory* accept-check on every combination that contains no construct it
//! is known to reject (`gap` below), because it does catch gross breakage; the
//! known rejections are pinned by [`sqlparser_gaps_are_still_real`] so a
//! stricter sqlparser tells us to promote them. The real judge is the server:
//! under `--features live-docker` every combination is `PREPARE`d on MySQL 8.4,
//! which resolves names against `tests/schema/mysql.sql` and enforces what no
//! grammar can.
//!
//! Failures are collected and reported together with the combination that
//! produced them, so one broken interaction does not hide the rest.
//!
//! The `exhaustive` feature widens variant dimensions (join kinds, compound
//! operators, lock spellings) from one representative to all of them.

use keelson_core::Mod;
use keelson_mysql as mysql;
use keelson_mysql::{
    Chain, Expr, Query, Value, arg, delete, insert, quote, replace, select, update, window,
};
use keelson_sqlcheck::{Dialect, live, normalize};

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/// One option of one clause dimension: how to apply it to the query under
/// construction, and what the manual says it must render as.
struct Piece<Q> {
    /// Names the option in failure reports and in `requires`/`conflicts`.
    /// Empty for the "clause absent" option.
    name: &'static str,
    apply: Box<dyn Fn(&mut Q)>,
    /// The fragment, from the statement's production in the manual. `#` marks a
    /// bound placeholder.
    frag: &'static str,
    /// The values the fragment binds, one per `#`, in its own emission order.
    vals: &'static [i32],
    /// Piece names that must also be selected for this combination to be a
    /// statement at all. The reason is documented where the piece is built.
    requires: &'static [&'static str],
    /// Piece names this one cannot legally combine with.
    conflicts: &'static [&'static str],
    /// `sqlparser` cannot parse this construct (see
    /// [`sqlparser_gaps_are_still_real`]), so combinations containing it skip
    /// the advisory grammar tier. The manual and the server are the judges.
    gap: bool,
}

impl<Q> Piece<Q> {
    fn new(name: &'static str, frag: &'static str, apply: impl Fn(&mut Q) + 'static) -> Piece<Q> {
        Piece {
            name,
            apply: Box::new(apply),
            frag,
            vals: &[],
            requires: &[],
            conflicts: &[],
            gap: false,
        }
    }

    fn vals(mut self, vals: &'static [i32]) -> Piece<Q> {
        self.vals = vals;
        self
    }

    fn requires(mut self, requires: &'static [&'static str]) -> Piece<Q> {
        self.requires = requires;
        self
    }

    fn conflicts(mut self, conflicts: &'static [&'static str]) -> Piece<Q> {
        self.conflicts = conflicts;
        self
    }

    fn gap(mut self) -> Piece<Q> {
        self.gap = true;
        self
    }
}

/// The "clause absent" option every optional dimension starts with.
fn none<Q>() -> Piece<Q> {
    Piece::new("", "", |_| {})
}

/// A toggle: the clause is absent or it is this.
fn toggle<Q>(piece: Piece<Q>) -> Vec<Piece<Q>> {
    vec![none(), piece]
}

/// Join fragments in manual order, replacing `#` with `?`. A fragment starting
/// with `,` glues to the previous one, because MySQL writes no space before a
/// comma.
fn assemble(parts: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (_, frag) in parts {
        if frag.is_empty() {
            continue;
        }
        if !out.is_empty() && !frag.starts_with(',') {
            out.push(' ');
        }
        out.push_str(frag);
    }
    out.replace('#', "?")
}

/// Run the full cross product of `dims` and return how many combinations were
/// checked. `assemble_fn` turns the selected `(name, fragment)` pairs into the
/// expected statement; every caller but the compound SELECT uses [`assemble`].
///
/// # Panics
/// At the end, listing every failing combination (capped for readability).
fn run<Q: Query + Clone>(
    what: &str,
    new: impl Fn() -> Q,
    dims: &[Vec<Piece<Q>>],
    assemble_fn: impl Fn(&[(&str, &str)]) -> String,
) -> usize {
    // Guard the guard: a typo'd `requires`/`conflicts` name would silently
    // exclude nothing (or everything), so every referenced name must exist.
    let all_names: Vec<&str> = dims
        .iter()
        .flatten()
        .map(|p| p.name)
        .filter(|n| !n.is_empty())
        .collect();
    for p in dims.iter().flatten() {
        for r in p.requires.iter().chain(p.conflicts) {
            assert!(
                all_names.contains(r),
                "{what}: piece {:?} references unknown piece {r:?}",
                p.name
            );
        }
    }

    let engine = live::available().contains(&Dialect::Mysql);
    let total: usize = dims.iter().map(Vec::len).product();
    let mut cases = 0;
    let mut failures: Vec<String> = Vec::new();

    for mut ix in 0..total {
        let selected: Vec<&Piece<Q>> = dims
            .iter()
            .map(|dim| {
                let p = &dim[ix % dim.len()];
                ix /= dim.len();
                p
            })
            .collect();

        let present: Vec<&str> = selected.iter().map(|p| p.name).collect();
        let legal = selected.iter().all(|p| {
            p.requires.iter().all(|r| present.contains(r))
                && !p.conflicts.iter().any(|c| present.contains(c))
        });
        if !legal {
            continue;
        }
        cases += 1;

        let combo = || {
            present
                .iter()
                .filter(|n| !n.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("+")
        };

        let mut q = new();
        for p in &selected {
            (p.apply)(&mut q);
        }

        let parts: Vec<(&str, &str)> = selected.iter().map(|p| (p.name, p.frag)).collect();
        let expected = assemble_fn(&parts);
        let expected_args: Vec<Value> = selected
            .iter()
            .flat_map(|p| p.vals.iter().map(|&v| Value::I32(v)))
            .collect();

        let (sql, args) = match q.build() {
            Ok(built) => built,
            Err(e) => {
                failures.push(format!("[{}] did not build: {e}", combo()));
                continue;
            }
        };

        // Advisory grammar tier: only where sqlparser has no known gap. A
        // gap-flagged case is still judged (string intent below, engine when
        // compiled in), so its SQL still belongs in Tier D's recording —
        // `check_mysql` records only on success, which a gap never reaches.
        let has_gap = selected.iter().any(|p| p.gap);
        if has_gap {
            keelson_sqlcheck::record(Dialect::Mysql, &sql);
        }
        if !has_gap && let Err(e) = keelson_sqlcheck::check_mysql(&sql) {
            failures.push(format!(
                "[{}] sqlparser rejected (no gap declared): {e}\n  sql: {sql}",
                combo()
            ));
            continue;
        }
        // The real judge, when this build can reach it.
        if engine && let Err(e) = live::check(Dialect::Mysql, &sql) {
            failures.push(format!("[{}] engine rejected: {e}\n  sql: {sql}", combo()));
            continue;
        }
        // The statement we meant: presence, absence and order of every clause.
        if normalize(&sql) != normalize(&expected) {
            failures.push(format!(
                "[{}] not the statement meant\n  expected: {}\n  actual:   {}",
                combo(),
                normalize(&expected),
                normalize(&sql)
            ));
            continue;
        }
        // Arguments bind in emission order — MySQL's whole numbering invariant.
        if args != expected_args {
            failures.push(format!(
                "[{}] arguments out of order\n  expected: {expected_args:?}\n  actual:   {args:?}",
                combo()
            ));
            continue;
        }
        // Deterministic: twice, and via a clone.
        let again = q.build().expect("second build of an already-built query");
        let cloned = q.clone().build().expect("build of a clone");
        if again.0 != sql || again.1 != args || cloned.0 != sql || cloned.1 != args {
            failures.push(format!("[{}] rendering is not deterministic", combo()));
        }
    }

    if !failures.is_empty() {
        let shown = failures.iter().take(15).cloned().collect::<Vec<_>>();
        panic!(
            "{what}: {} of {cases} combinations failed\n{}",
            failures.len(),
            shown.join("\n")
        );
    }
    println!("{what}: {cases} combinations checked");
    cases
}

// ---------------------------------------------------------------------------
// The declared sqlparser gaps, pinned
// ---------------------------------------------------------------------------

/// Every construct the pieces below mark `gap` is asserted here to still be one,
/// on a minimal statement. If sqlparser learns one, this fires and the flag
/// should come off, promoting those combinations back to the advisory tier.
#[test]
fn sqlparser_gaps_are_still_real() {
    let gaps = [
        (
            "GROUP BY … WITH ROLLUP",
            "SELECT `status` FROM `posts` GROUP BY `status` WITH ROLLUP",
        ),
        (
            "LOCK IN SHARE MODE",
            "SELECT `id` FROM `users` LOCK IN SHARE MODE",
        ),
        // sqlparser's UPDATE-modifier support is partial in a way that only
        // shows in combination: `UPDATE IGNORE tbl` and `UPDATE LOW_PRIORITY
        // tbl` parse on their own, but a modifier followed by an aliased table
        // does not, and nor do the two modifiers together. That is why every
        // UPDATE modifier piece is flagged `gap` rather than just IGNORE.
        (
            "an UPDATE modifier before an aliased table",
            "UPDATE LOW_PRIORITY `users` AS `u` SET `age` = 1",
        ),
        (
            "the UPDATE … LOW_PRIORITY IGNORE modifier pair",
            "UPDATE LOW_PRIORITY IGNORE `users` SET `age` = 1",
        ),
        (
            "multiple-table UPDATE through a comma list",
            "UPDATE `users` AS `u`, `posts` AS `p` SET `p`.`views` = 1",
        ),
        // Like UPDATE, DELETE's gap is emergent: each modifier parses alone,
        // the full LOW_PRIORITY QUICK IGNORE run does not.
        (
            "the DELETE … LOW_PRIORITY QUICK IGNORE modifier run",
            "DELETE LOW_PRIORITY QUICK IGNORE FROM `comments`",
        ),
    ];
    for (what, sql) in gaps {
        assert!(
            keelson_sqlcheck::check_mysql(sql).is_err(),
            "sqlparser now accepts {what} — drop the `gap` flag on its piece"
        );
    }
}

// ---------------------------------------------------------------------------
// Shared pieces
// ---------------------------------------------------------------------------

/// The one-CTE prefix. MySQL allows an unreferenced CTE, so nothing downstream
/// needs to name it; its bound value renders first, which is the point — every
/// later argument shifts by one.
macro_rules! with_piece {
    ($module:ident) => {
        toggle(
            Piece::new(
                "with",
                "WITH `recent` AS (SELECT `id` FROM `posts` WHERE (`views` > #))",
                |q| {
                    $module::with(
                        "recent",
                        mysql::select((
                            select::columns(quote("id")),
                            select::from(quote("posts")),
                            select::where_(quote("views").gt(arg(101i32))),
                        )),
                    )
                    .apply(q)
                },
            )
            .vals(&[101]),
        )
    };
}

// ---------------------------------------------------------------------------
// SELECT — manual 15.2.13:
//
// [WITH …] SELECT [/*+ hint */] [DISTINCT] select_expr [FROM table_references]
// [WHERE] [GROUP BY [WITH ROLLUP]] [HAVING] [WINDOW] [ORDER BY]
// [LIMIT [OFFSET]] [FOR UPDATE | FOR SHARE | LOCK IN SHARE MODE]
// ---------------------------------------------------------------------------

/// The core clauses, no set operation — the widest product.
#[test]
fn select_every_clause_against_every_other() {
    let head = vec![Piece::new("", "SELECT", |_| {})];
    let hint = toggle(Piece::new(
        "hint",
        "/*+ MAX_EXECUTION_TIME(1000) */",
        |q: &mut mysql::SelectQuery| select::max_execution_time(1000).apply(q),
    ));
    let mut distinct = vec![
        none(),
        Piece::new("distinct", "DISTINCT", |q| select::distinct().apply(q)),
    ];
    if cfg!(feature = "exhaustive") {
        distinct.push(Piece::new("distinctrow", "DISTINCTROW", |q| {
            select::distinct_row().apply(q)
        }));
    }
    // A fixed one-column projection: every ORDER BY term below names it, and
    // ONLY_FULL_GROUP_BY is satisfied whenever GROUP BY groups by it.
    let columns = vec![Piece::new("", "`name`", |q| {
        select::columns(quote("name")).apply(q)
    })];
    // The index hint decorates the from-item, so they are one dimension.
    let from = vec![
        Piece::new("", "FROM `users`", |q| {
            select::from(quote("users")).apply(q)
        }),
        Piece::new("use_index", "FROM `users` USE INDEX (`PRIMARY`)", |q| {
            select::from(quote("users")).use_index(["PRIMARY"]).apply(q)
        }),
    ];

    let mut join = vec![
        none(),
        Piece::new(
            "inner_join",
            "INNER JOIN `posts` ON (`posts`.`user_id` = `users`.`id`)",
            |q| {
                select::inner_join(quote("posts"))
                    .on_eq(quote(("posts", "user_id")), quote(("users", "id")))
                    .apply(q)
            },
        ),
    ];
    if cfg!(feature = "exhaustive") {
        join.push(Piece::new(
            "left_join",
            "LEFT JOIN `posts` ON (`posts`.`user_id` = `users`.`id`)",
            |q| {
                select::left_join(quote("posts"))
                    .on_eq(quote(("posts", "user_id")), quote(("users", "id")))
                    .apply(q)
            },
        ));
        // The join operator, not the statement modifier of the same spelling.
        join.push(Piece::new(
            "straight_join",
            "STRAIGHT_JOIN `posts` ON (`posts`.`user_id` = `users`.`id`)",
            |q| {
                select::straight_join(quote("posts"))
                    .on_eq(quote(("posts", "user_id")), quote(("users", "id")))
                    .apply(q)
            },
        ));
    }

    let where_ = toggle(
        Piece::new("where", "WHERE (`age` >= #)", |q| {
            select::where_(quote("age").gte(arg(21i32))).apply(q)
        })
        .vals(&[21]),
    );
    let group_by = toggle(Piece::new("group_by", "GROUP BY `name`", |q| {
        select::group_by(quote("name")).apply(q)
    }));
    // `WITH ROLLUP` is a decoration of GROUP BY, meaningless without one.
    let rollup = toggle(
        Piece::new("rollup", "WITH ROLLUP", |q| select::with_rollup().apply(q))
            .requires(&["group_by"])
            .gap(),
    );
    // Under ONLY_FULL_GROUP_BY (the 8.4 default) a HAVING makes the query an
    // aggregate, and the plain-column projection is then only legal grouped —
    // so in this product HAVING needs GROUP BY.
    let having = toggle(
        Piece::new("having", "HAVING (COUNT(*) > #)", |q| {
            select::having(Expr::func("COUNT", "*").gt(arg(3i32))).apply(q)
        })
        .vals(&[3])
        .requires(&["group_by"]),
    );
    // A named window nothing references is still a WINDOW clause; partitioning
    // by the projected column keeps ONLY_FULL_GROUP_BY satisfied when grouped.
    let window_ = toggle(Piece::new(
        "window",
        "WINDOW `w` AS (PARTITION BY `name`)",
        |q| select::window("w", window::partition_by(quote("name"))).apply(q),
    ));
    let order_by = toggle(Piece::new("order_by", "ORDER BY `name` DESC", |q| {
        select::order_by(quote("name")).desc().apply(q)
    }));
    let limit =
        toggle(Piece::new("limit", "LIMIT #", |q| select::limit(arg(10i32)).apply(q)).vals(&[10]));
    // `LIMIT [offset,] row_count`: the grammar spells the pair as one clause,
    // so OFFSET alone is not a statement.
    let offset = toggle(
        Piece::new("offset", "OFFSET #", |q| select::offset(arg(5i32)).apply(q))
            .vals(&[5])
            .requires(&["limit"]),
    );

    let mut lock = vec![
        none(),
        Piece::new("for_update", "FOR UPDATE", |q| {
            select::for_update().apply(q)
        }),
        Piece::new("lock_in_share_mode", "LOCK IN SHARE MODE", |q| {
            select::lock_in_share_mode().apply(q)
        })
        .gap(),
    ];
    if cfg!(feature = "exhaustive") {
        lock.push(Piece::new(
            "for_update_of_nowait",
            "FOR UPDATE OF `users` NOWAIT",
            |q| select::for_update().of(["users"]).no_wait().apply(q),
        ));
        lock.push(Piece::new(
            "for_share_skip_locked",
            "FOR SHARE SKIP LOCKED",
            |q| select::for_share().skip_locked().apply(q),
        ));
    }

    let cases = run(
        "mysql SELECT",
        || mysql::select(()),
        &[
            head, hint, distinct, columns, from, join, where_, group_by, rollup, having, window_,
            order_by, limit, offset, lock,
        ],
        assemble,
    );
    assert!(cases >= 3888, "the cross product shrank: {cases}");
}

/// The set-operation clauses crossed with the CTE prefix and both tail
/// positions — a query block's own ORDER BY/LIMIT against the compound's.
///
/// The manual (15.2.14) requires a query block that carries its own trailing
/// clauses to be parenthesised before a set operator, so the assembler wraps
/// the core exactly when a compound meets a direct tail clause.
#[test]
fn select_compounds_against_both_tail_positions() {
    let with = with_piece!(select);
    let head = vec![Piece::new("", "SELECT", |_| {})];
    let distinct = toggle(Piece::new("distinct", "DISTINCT", |q| {
        select::distinct().apply(q)
    }));
    let columns = vec![Piece::new("", "`name`", |q| {
        select::columns(quote("name")).apply(q)
    })];
    let from = vec![Piece::new("", "FROM `users`", |q| {
        select::from(quote("users")).apply(q)
    })];
    let where_ = toggle(
        Piece::new("where", "WHERE (`age` >= #)", |q| {
            select::where_(quote("age").gte(arg(21i32))).apply(q)
        })
        .vals(&[21]),
    );

    fn second_core() -> mysql::SelectQuery {
        mysql::select((
            select::columns(quote("title")),
            select::from(quote("posts")),
        ))
    }
    let mut compound = vec![
        none(),
        Piece::new("union", "UNION (SELECT `title` FROM `posts`)", |q| {
            select::union(second_core()).apply(q)
        }),
    ];
    if cfg!(feature = "exhaustive") {
        compound.push(Piece::new(
            "union_all",
            "UNION ALL (SELECT `title` FROM `posts`)",
            |q| select::union_all(second_core()).apply(q),
        ));
        compound.push(Piece::new(
            "intersect",
            "INTERSECT (SELECT `title` FROM `posts`)",
            |q| select::intersect(second_core()).apply(q),
        ));
        compound.push(Piece::new(
            "except",
            "EXCEPT (SELECT `title` FROM `posts`)",
            |q| select::except(second_core()).apply(q),
        ));
    }

    // The query block's own tail clauses.
    let order_by = toggle(Piece::new("order_by", "ORDER BY `name` DESC", |q| {
        select::order_by(quote("name")).desc().apply(q)
    }));
    let limit =
        toggle(Piece::new("limit", "LIMIT #", |q| select::limit(arg(10i32)).apply(q)).vals(&[10]));
    let offset = toggle(
        Piece::new("offset", "OFFSET #", |q| select::offset(arg(5i32)).apply(q))
            .vals(&[5])
            .requires(&["limit"]),
    );

    // The compound's tail clauses. `requires` is all-of, so each combined
    // piece pins itself to the `union` operator specifically; the exhaustive
    // operator variants are exercised without combined tails, which the plain
    // grammar walk covers one at a time.
    let compound_present: &[&str] = &["union", "union_all", "intersect", "except"];
    let c_order = toggle(
        Piece::new("c_order", "ORDER BY `name`", |q| {
            select::order_by_combined(quote("name")).apply(q)
        })
        .requires(&["union"]),
    );
    let c_limit = toggle(
        Piece::new("c_limit", "LIMIT #", |q| {
            select::limit_combined(arg(11i32)).apply(q)
        })
        .vals(&[11])
        .requires(&["union"]),
    );
    let c_offset = toggle(
        Piece::new("c_offset", "OFFSET #", |q| {
            select::offset_combined(arg(6i32)).apply(q)
        })
        .vals(&[6])
        .requires(&["union", "c_limit"]),
    );

    // Wrap the core in parentheses exactly when a compound meets a direct tail.
    let assemble_compound = |parts: &[(&str, &str)]| -> String {
        let has_compound = parts.iter().any(|(n, _)| compound_present.contains(n));
        let has_tail = parts
            .iter()
            .any(|(n, _)| ["order_by", "limit", "offset"].contains(n));
        if !(has_compound && has_tail) {
            return assemble(parts);
        }
        let is_suffix = |n: &str| {
            compound_present.contains(&n) || ["c_order", "c_limit", "c_offset"].contains(&n)
        };
        let prefix: Vec<(&str, &str)> = parts
            .iter()
            .copied()
            .filter(|(n, _)| *n == "with")
            .collect();
        let core: Vec<(&str, &str)> = parts
            .iter()
            .copied()
            .filter(|(n, _)| *n != "with" && !is_suffix(n))
            .collect();
        let suffix: Vec<(&str, &str)> = parts
            .iter()
            .copied()
            .filter(|(n, _)| is_suffix(n))
            .collect();
        let mut out = assemble(&prefix);
        if !out.is_empty() {
            out.push(' ');
        }
        out.push('(');
        out.push_str(&assemble(&core));
        out.push(')');
        let tail = assemble(&suffix);
        if !tail.is_empty() {
            out.push(' ');
            out.push_str(&tail);
        }
        out
    };

    let cases = run(
        "mysql SELECT compounds",
        || mysql::select(()),
        &[
            with, head, distinct, columns, from, where_, compound, order_by, limit, offset,
            c_order, c_limit, c_offset,
        ],
        assemble_compound,
    );
    assert!(cases >= 264, "the cross product shrank: {cases}");
}

// ---------------------------------------------------------------------------
// INSERT — manual 15.2.7:
//
// INSERT [LOW_PRIORITY | HIGH_PRIORITY] [IGNORE] INTO tbl [(cols)]
// { VALUES … | SET … | SELECT … } [AS row_alias [(col_alias …)]]
// [ON DUPLICATE KEY UPDATE assignment_list]
//
// No WITH (a CTE goes on the sub-SELECT), no RETURNING — MySQL has neither.
// ---------------------------------------------------------------------------

#[test]
fn insert_every_clause_against_every_other() {
    let head = vec![Piece::new("", "INSERT", |_| {})];
    let hint = toggle(Piece::new(
        "hint",
        "/*+ MAX_EXECUTION_TIME(1000) */",
        |q: &mut mysql::InsertQuery| insert::max_execution_time(1000).apply(q),
    ));
    let modifier = vec![
        none(),
        Piece::new("low_priority", "LOW_PRIORITY", |q| {
            insert::low_priority().apply(q)
        }),
        Piece::new("high_priority", "HIGH_PRIORITY", |q| {
            insert::high_priority().apply(q)
        }),
        Piece::new("ignore", "IGNORE", |q| insert::ignore().apply(q)),
        Piece::new("high_priority_ignore", "HIGH_PRIORITY IGNORE", |q| {
            insert::high_priority().apply(q);
            insert::ignore().apply(q);
        }),
    ];

    // The row source decides the column list too, so they are one dimension.
    let source = vec![
        Piece::new("values", "INTO `tags` (`id`, `name`) VALUES (#, #)", |q| {
            insert::into(quote("tags")).columns(["id", "name"]).apply(q);
            insert::values((arg(1i32), arg(2i32))).apply(q);
        })
        .vals(&[1, 2]),
        Piece::new(
            "values_rows",
            "INTO `tags` (`id`, `name`) VALUES (#, #), (#, #)",
            |q| {
                insert::into(quote("tags")).columns(["id", "name"]).apply(q);
                insert::values((arg(1i32), arg(2i32))).apply(q);
                insert::values((arg(3i32), arg(4i32))).apply(q);
            },
        )
        .vals(&[1, 2, 3, 4]),
        Piece::new("set_source", "INTO `tags` SET `id` = #, `name` = #", |q| {
            insert::into(quote("tags")).apply(q);
            insert::set_col("id").to_arg(1i32).apply(q);
            insert::set_col("name").to_arg(2i32).apply(q);
        })
        .vals(&[1, 2]),
        Piece::new(
            "select_source",
            "INTO `tags` (`id`, `name`) SELECT `id`, `title` FROM `posts`",
            |q| {
                insert::into(quote("tags")).columns(["id", "name"]).apply(q);
                insert::query(mysql::select((
                    select::columns((quote("id"), quote("title"))),
                    select::from(quote("posts")),
                )))
                .apply(q);
            },
        ),
    ];

    // The 8.0.19 row alias hangs off the VALUES list in keelson's rendering, so
    // it requires a VALUES source. `alias_cols` renames the columns, after
    // which the row can only be reached through the new names.
    let alias = vec![
        none(),
        Piece::new("alias", "AS `new`", |q| insert::as_("new").apply(q)).requires(&["values"]),
        Piece::new("alias_cols", "AS `new` (`new_id`, `new_name`)", |q| {
            insert::as_("new").columns(["new_id", "new_name"]).apply(q)
        })
        .requires(&["values"]),
    ];

    let odku = vec![
        none(),
        Piece::new("odku", "ON DUPLICATE KEY UPDATE `name` = #", |q| {
            insert::on_duplicate_key_update(insert::set_col("name").to_arg(9i32)).apply(q)
        })
        .vals(&[9]),
        // `VALUES(col)` is the pre-8.0.19 spelling and cannot be mixed with a
        // row alias — the server refuses the combination outright.
        Piece::new(
            "odku_values",
            "ON DUPLICATE KEY UPDATE `name` = VALUES(`name`)",
            |q| insert::on_duplicate_key_update(insert::set_values(["name"])).apply(q),
        )
        .conflicts(&["alias", "alias_cols"]),
        Piece::new(
            "odku_row",
            "ON DUPLICATE KEY UPDATE `name` = `new`.`name`",
            |q| insert::on_duplicate_key_update(insert::set_row("new", ["name"])).apply(q),
        )
        .requires(&["alias"]),
    ];

    let cases = run(
        "mysql INSERT",
        || mysql::insert(()),
        &[head, hint, modifier, source, alias, odku],
        assemble,
    );
    assert!(cases >= 170, "the cross product shrank: {cases}");
}

// ---------------------------------------------------------------------------
// REPLACE — manual 15.2.12:
//
// REPLACE [LOW_PRIORITY | DELAYED] INTO tbl [(cols)] { VALUES … | SET … | SELECT … }
//
// Deliberately short: REPLACE has no IGNORE, no HIGH_PRIORITY, no row alias and
// no ON DUPLICATE KEY UPDATE, and the mods for those do not compile against it.
// ---------------------------------------------------------------------------

#[test]
fn replace_every_clause_against_every_other() {
    let head = vec![Piece::new("", "REPLACE", |_| {})];
    let hint = toggle(Piece::new(
        "hint",
        "/*+ MAX_EXECUTION_TIME(1000) */",
        |q: &mut mysql::ReplaceQuery| replace::max_execution_time(1000).apply(q),
    ));
    let modifier = vec![
        none(),
        Piece::new("low_priority", "LOW_PRIORITY", |q| {
            replace::low_priority().apply(q)
        }),
    ];
    let source = vec![
        Piece::new("values", "INTO `tags` (`id`, `name`) VALUES (#, #)", |q| {
            replace::into(quote("tags"))
                .columns(["id", "name"])
                .apply(q);
            replace::values((arg(1i32), arg(2i32))).apply(q);
        })
        .vals(&[1, 2]),
        Piece::new("set_source", "INTO `tags` SET `id` = #, `name` = #", |q| {
            replace::into(quote("tags")).apply(q);
            replace::set_col("id").to_arg(1i32).apply(q);
            replace::set_col("name").to_arg(2i32).apply(q);
        })
        .vals(&[1, 2]),
        Piece::new(
            "select_source",
            "INTO `tags` (`id`, `name`) SELECT `id`, `title` FROM `posts`",
            |q| {
                replace::into(quote("tags"))
                    .columns(["id", "name"])
                    .apply(q);
                replace::query(mysql::select((
                    select::columns((quote("id"), quote("title"))),
                    select::from(quote("posts")),
                )))
                .apply(q);
            },
        ),
    ];

    let cases = run(
        "mysql REPLACE",
        || mysql::replace(()),
        &[head, hint, modifier, source],
        assemble,
    );
    assert!(cases >= 12, "the cross product shrank: {cases}");
}

// ---------------------------------------------------------------------------
// UPDATE — manual 15.2.17:
//
// [WITH …] UPDATE [LOW_PRIORITY] [IGNORE] table_reference
// SET assignment_list [WHERE] [ORDER BY] [LIMIT]
//
// ORDER BY and LIMIT belong to the single-table form only; the multi-table
// interactions get their own product below.
// ---------------------------------------------------------------------------

#[test]
fn update_single_table_every_clause_against_every_other() {
    let with = with_piece!(update);
    let head = vec![Piece::new("", "UPDATE", |_| {})];
    let hint = toggle(Piece::new(
        "hint",
        "/*+ MAX_EXECUTION_TIME(1000) */",
        |q: &mut mysql::UpdateQuery| update::max_execution_time(1000).apply(q),
    ));
    let modifier = vec![
        none(),
        // All three carry `gap`: see sqlparser_gaps_are_still_real for the
        // combinations sqlparser cannot parse.
        Piece::new("low_priority", "LOW_PRIORITY", |q| {
            update::low_priority().apply(q)
        })
        .gap(),
        Piece::new("ignore", "IGNORE", |q| update::ignore().apply(q)).gap(),
        Piece::new("low_priority_ignore", "LOW_PRIORITY IGNORE", |q| {
            update::low_priority().apply(q);
            update::ignore().apply(q);
        })
        .gap(),
    ];
    // The index hint decorates the target, so they are one dimension.
    let target = vec![
        Piece::new("target", "`users`", |q| {
            update::table(quote("users")).apply(q)
        }),
        Piece::new("target_alias", "`users` AS `u`", |q| {
            update::table(quote("users")).as_("u").apply(q)
        }),
        Piece::new(
            "target_force_index",
            "`users` FORCE INDEX (`PRIMARY`)",
            |q| {
                update::table(quote("users"))
                    .force_index(["PRIMARY"])
                    .apply(q)
            },
        ),
    ];
    let set = vec![
        Piece::new("", "SET `age` = #", |q| {
            update::set_col("age").to_arg(7i32).apply(q)
        })
        .vals(&[7]),
    ];
    let where_ = toggle(
        Piece::new("where", "WHERE (`age` < #)", |q| {
            update::where_(quote("age").lt(arg(21i32))).apply(q)
        })
        .vals(&[21]),
    );
    let order_by = toggle(Piece::new("order_by", "ORDER BY `id` DESC", |q| {
        update::order_by(quote("id")).desc().apply(q)
    }));
    let limit =
        toggle(Piece::new("limit", "LIMIT #", |q| update::limit(arg(10i32)).apply(q)).vals(&[10]));

    let cases = run(
        "mysql UPDATE single-table",
        || mysql::update(()),
        &[
            with, head, hint, modifier, target, set, where_, order_by, limit,
        ],
        assemble,
    );
    assert!(cases >= 384, "the cross product shrank: {cases}");
}

/// The multiple-table form: a comma list or a join, no ORDER BY, no LIMIT —
/// MySQL rejects both as soon as more than one table is named.
#[test]
fn update_multi_table_every_clause_against_every_other() {
    let with = with_piece!(update);
    let head = vec![Piece::new("", "UPDATE", |_| {})];
    let hint = toggle(Piece::new(
        "hint",
        "/*+ MAX_EXECUTION_TIME(1000) */",
        |q: &mut mysql::UpdateQuery| update::max_execution_time(1000).apply(q),
    ));
    let tables = vec![
        // The comma list is one of the two constructs sqlparser is known to
        // reject outright — the reason MySQL's Tier 1 is advisory at all.
        Piece::new("comma_list", "`users` AS `u`, `posts` AS `p`", |q| {
            update::table(quote("users")).as_("u").apply(q);
            update::table_also(quote("posts")).as_("p").apply(q);
        })
        .gap(),
        Piece::new(
            "inner_join",
            "`users` AS `u` INNER JOIN `posts` AS `p` ON (`p`.`user_id` = `u`.`id`)",
            |q| {
                update::table(quote("users")).as_("u").apply(q);
                update::inner_join(quote("posts"))
                    .as_("p")
                    .on_eq(quote(("p", "user_id")), quote(("u", "id")))
                    .apply(q);
            },
        ),
        Piece::new(
            "left_join",
            "`users` AS `u` LEFT JOIN `posts` AS `p` ON (`p`.`user_id` = `u`.`id`)",
            |q| {
                update::table(quote("users")).as_("u").apply(q);
                update::left_join(quote("posts"))
                    .as_("p")
                    .on_eq(quote(("p", "user_id")), quote(("u", "id")))
                    .apply(q);
            },
        ),
        Piece::new(
            "straight_join",
            "`users` AS `u` STRAIGHT_JOIN `posts` AS `p` ON (`p`.`user_id` = `u`.`id`)",
            |q| {
                update::table(quote("users")).as_("u").apply(q);
                update::straight_join(quote("posts"))
                    .as_("p")
                    .on_eq(quote(("p", "user_id")), quote(("u", "id")))
                    .apply(q);
            },
        ),
    ];
    let set = vec![
        Piece::new("", "SET `p`.`views` = #", |q| {
            update::set_col(("p", "views")).to_arg(7i32).apply(q)
        })
        .vals(&[7]),
    ];
    let where_ = toggle(
        Piece::new("where", "WHERE (`u`.`age` > #)", |q| {
            update::where_(quote(("u", "age")).gt(arg(21i32))).apply(q)
        })
        .vals(&[21]),
    );

    let cases = run(
        "mysql UPDATE multi-table",
        || mysql::update(()),
        &[with, head, hint, tables, set, where_],
        assemble,
    );
    assert!(cases >= 32, "the cross product shrank: {cases}");
}

// ---------------------------------------------------------------------------
// DELETE — manual 15.2.2:
//
// [WITH …] DELETE [LOW_PRIORITY] [QUICK] [IGNORE] FROM tbl [AS alias]
// [WHERE] [ORDER BY] [LIMIT]
// ---------------------------------------------------------------------------

#[test]
fn delete_single_table_every_clause_against_every_other() {
    let with = with_piece!(delete);
    let head = vec![Piece::new("", "DELETE", |_| {})];
    let hint = toggle(Piece::new(
        "hint",
        "/*+ MAX_EXECUTION_TIME(1000) */",
        |q: &mut mysql::DeleteQuery| delete::max_execution_time(1000).apply(q),
    ));
    let modifier = vec![
        none(),
        Piece::new("low_priority", "LOW_PRIORITY", |q| {
            delete::low_priority().apply(q)
        }),
        Piece::new("quick", "QUICK", |q| delete::quick().apply(q)),
        Piece::new("ignore", "IGNORE", |q| delete::ignore().apply(q)),
        Piece::new("all_modifiers", "LOW_PRIORITY QUICK IGNORE", |q| {
            delete::low_priority().apply(q);
            delete::quick().apply(q);
            delete::ignore().apply(q);
        })
        .gap(),
    ];
    let target = vec![
        Piece::new("target", "FROM `comments`", |q| {
            delete::from(quote("comments")).apply(q)
        }),
        Piece::new("target_alias", "FROM `comments` AS `c`", |q| {
            delete::from(quote("comments")).as_("c").apply(q)
        }),
    ];
    let where_ = toggle(
        Piece::new("where", "WHERE (`id` = #)", |q| {
            delete::where_(quote("id").eq(arg(21i32))).apply(q)
        })
        .vals(&[21]),
    );
    let order_by = toggle(Piece::new("order_by", "ORDER BY `id` DESC", |q| {
        delete::order_by(quote("id")).desc().apply(q)
    }));
    let limit =
        toggle(Piece::new("limit", "LIMIT #", |q| delete::limit(arg(10i32)).apply(q)).vals(&[10]));

    let cases = run(
        "mysql DELETE single-table",
        || mysql::delete(()),
        &[with, head, hint, modifier, target, where_, order_by, limit],
        assemble,
    );
    assert!(cases >= 320, "the cross product shrank: {cases}");
}

/// The multiple-table form: `FROM … USING …`, joins on the USING list, no
/// ORDER BY, no LIMIT.
#[test]
fn delete_multi_table_every_clause_against_every_other() {
    let with = with_piece!(delete);
    let head = vec![Piece::new("", "DELETE", |_| {})];
    let form = vec![
        Piece::new(
            "using_comma",
            "FROM `comments` USING `comments`, `posts` WHERE (`comments`.`post_id` = `posts`.`id`)",
            |q| {
                delete::from(quote("comments")).apply(q);
                delete::using(quote("comments")).apply(q);
                delete::using_also(quote("posts")).apply(q);
                delete::where_(quote(("comments", "post_id")).eq(quote(("posts", "id")))).apply(q);
            },
        ),
        Piece::new(
            "using_join",
            "FROM `comments` USING `comments` INNER JOIN `posts` ON (`posts`.`id` = `comments`.`post_id`) WHERE (`posts`.`status` = #)",
            |q| {
                delete::from(quote("comments")).apply(q);
                delete::using(quote("comments")).apply(q);
                delete::inner_join(quote("posts"))
                    .on_eq(quote(("posts", "id")), quote(("comments", "post_id")))
                    .apply(q);
                delete::where_(quote(("posts", "status")).eq(arg(21i32))).apply(q);
            },
        )
        .vals(&[21]),
        Piece::new(
            "from_list",
            "FROM `comments`, `post_tags` USING `comments` INNER JOIN `post_tags` ON (`post_tags`.`post_id` = `comments`.`post_id`)",
            |q| {
                delete::from(quote("comments")).apply(q);
                delete::from(quote("post_tags")).apply(q);
                delete::using(quote("comments")).apply(q);
                delete::inner_join(quote("post_tags"))
                    .on_eq(quote(("post_tags", "post_id")), quote(("comments", "post_id")))
                    .apply(q);
            },
        ),
    ];

    let cases = run(
        "mysql DELETE multi-table",
        || mysql::delete(()),
        &[with, head, form],
        assemble,
    );
    assert!(cases >= 6, "the cross product shrank: {cases}");
}

/// The twin of the join-mods guard: the extra table references of
/// `from_also` / `using_also` are second and later entries of the list the
/// leading item opens, so with no leading item they used to be dropped
/// silently — valid SQL, the caller's item simply gone. Now `build()` refuses
/// (DEV-201). `UPDATE` diverges the same way it did for joins: its target list
/// *is* the `table_references`, and an absent target is already its own
/// `Incomplete` before the list writer runs, so `table_also` can never be
/// dropped — the error names the missing target instead.
#[test]
fn extra_table_refs_without_a_leading_item_are_a_build_error() {
    let q = mysql::select((
        select::columns(quote("id")),
        select::from_also(quote("users")),
    ));
    let err = q.build().unwrap_err();
    // The substrings name the SQL concepts (the missing leading FROM / USING
    // item, the missing UPDATE target), not the message wording.
    assert!(
        matches!(&err, mysql::Error::Incomplete(what) if what.contains("FROM")),
        "got: {err}"
    );

    let q = mysql::delete((
        delete::from(quote("comments")),
        delete::using_also(quote("posts")),
    ));
    let err = q.build().unwrap_err();
    assert!(
        matches!(&err, mysql::Error::Incomplete(what) if what.contains("USING")),
        "got: {err}"
    );

    let q = mysql::update((
        update::table_also(quote("posts")),
        update::set_col("views").to(arg(1i32)),
    ));
    let err = q.build().unwrap_err();
    assert!(
        matches!(&err, mysql::Error::Incomplete(what) if what.contains("UPDATE")),
        "got: {err}"
    );
}
