//! Combinatorial clause coverage: every optional clause crossed with every other.
//!
//! The per-clause grammar walks (`grammar_*.rs`) test each clause once, next to
//! nothing. What they cannot catch is an *interaction*: a clause that renders in
//! the wrong order once another is present, or — the silent one — a placeholder
//! that binds the wrong value because a clause that renders earlier claimed its
//! number. A clause tested alone always starts at `?1`, so only a cross product
//! sees that.
//!
//! # Where the expectation comes from
//!
//! A hand-written expected string per combination is impossible, and pasting the
//! builder's output would make the suite assert that the code equals itself.
//! Instead each optional clause carries a **fragment derived from its production
//! in the SQLite syntax diagrams** (<https://www.sqlite.org/lang.html>), and the
//! expected statement is those fragments concatenated in the *diagram's* clause
//! order — which the builder does not get to choose. `#` marks a bound
//! placeholder; the assembler numbers them `?1..?n` in emission order, which is
//! exactly the numbering the writer must produce.
//!
//! Every combination is then held to all of:
//!
//! 1. **The grammar accepts it** — `sqlite3_parser`, a C→Rust port of SQLite's
//!    own `parse.y`, authoritative for this dialect.
//! 2. **A real SQLite accepts it** — `prepare` on the linked-in engine resolves
//!    every table and column against `tests/schema/sqlite.sql`. SQLite is a
//!    library, so this tier runs on a plain `cargo test`, no Docker; that is why
//!    this suite leans on the engine harder than any other dialect can afford to.
//! 3. **It is the statement we meant** — equality with the assembled expectation,
//!    which pins clause order, placeholder numbering and everything else.
//! 4. **The arguments bind in emission order** — every clause binds distinct
//!    values, so a swapped pair cannot cancel out.
//! 5. **Rendering is deterministic** — building twice and building a clone give
//!    byte-identical SQL and arguments.
//!
//! Failures are collected and reported together with the combination that
//! produced them, so one broken interaction does not hide the rest.
//!
//! The `exhaustive` feature widens variant dimensions (join kinds, compound
//! operators, ordering decorations) from one representative to all of them.

use keelson_core::Mod;
use keelson_sqlcheck::{Dialect, live, normalize};
use keelson_sqlite as sqlite;
use keelson_sqlite::{
    Chain, Expr, Query, Value, arg, delete, insert, quote, select, update, window,
};

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/// One option of one clause dimension: how to apply it to the query under
/// construction, and what the grammar says it must render as.
struct Piece<Q> {
    /// Names the option in failure reports and in `requires`/`conflicts`.
    /// Empty for the "clause absent" option.
    name: &'static str,
    apply: Box<dyn Fn(&mut Q)>,
    /// The fragment, from the clause's syntax diagram. `#` marks a placeholder.
    frag: &'static str,
    /// The values the fragment binds, one per `#`, in its own emission order.
    vals: &'static [i32],
    /// Piece names that must also be selected for this combination to be a
    /// statement at all. The reason is documented where the piece is built.
    requires: &'static [&'static str],
    /// Piece names this one cannot legally combine with.
    conflicts: &'static [&'static str],
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
}

/// The "clause absent" option every optional dimension starts with.
fn none<Q>() -> Piece<Q> {
    Piece::new("", "", |_| {})
}

/// Number the `#` markers `?1..?n` in emission order — the numbering invariant
/// itself, stated in the expectation rather than checked after the fact.
fn number_placeholders(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut n = 0;
    for ch in sql.chars() {
        if ch == '#' {
            n += 1;
            out.push_str(&format!("?{n}"));
        } else {
            out.push(ch);
        }
    }
    out
}

/// Join fragments in diagram order. A fragment starting with `,` glues to the
/// previous one, because SQLite writes no space before a comma.
fn assemble(frags: &[&str]) -> String {
    let mut out = String::new();
    for frag in frags {
        if frag.is_empty() {
            continue;
        }
        if !out.is_empty() && !frag.starts_with(',') {
            out.push(' ');
        }
        out.push_str(frag);
    }
    out
}

/// Run the full cross product of `dims` and return how many combinations were
/// checked.
///
/// # Panics
/// At the end, listing every failing combination (capped for readability).
fn run<Q: Query + Clone>(what: &str, new: impl Fn() -> Q, dims: &[Vec<Piece<Q>>]) -> usize {
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

        let frags: Vec<&str> = selected.iter().map(|p| p.frag).collect();
        let expected = number_placeholders(&assemble(&frags));
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

        // 1. SQLite's own grammar, ported.
        if let Err(e) = keelson_sqlcheck::check(Dialect::Sqlite, &sql) {
            failures.push(format!("[{}] grammar rejected: {e}\n  sql: {sql}", combo()));
            continue;
        }
        // 2. A real SQLite, names resolved against the shared schema.
        if let Err(e) = live::check_sqlite(&sql) {
            failures.push(format!("[{}] engine rejected: {e}\n  sql: {sql}", combo()));
            continue;
        }
        // 3. The statement we meant, clause order and numbering included.
        if normalize(&sql) != normalize(&expected) {
            failures.push(format!(
                "[{}] not the statement meant\n  expected: {}\n  actual:   {}",
                combo(),
                normalize(&expected),
                normalize(&sql)
            ));
            continue;
        }
        // 4. Arguments bind in emission order, values distinct per clause.
        if args != expected_args {
            failures.push(format!(
                "[{}] arguments out of order\n  expected: {expected_args:?}\n  actual:   {args:?}",
                combo()
            ));
            continue;
        }
        // 5. Deterministic: twice, and via a clone.
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

/// A toggle: the clause is absent or it is this.
fn toggle<Q>(piece: Piece<Q>) -> Vec<Piece<Q>> {
    vec![none(), piece]
}

// ---------------------------------------------------------------------------
// SELECT — <https://www.sqlite.org/lang_select.html>
//
// [ WITH … ] SELECT [ DISTINCT ] result-column [ FROM … join-clause ]
// [ WHERE ] [ GROUP BY [ HAVING ] ] [ WINDOW ]
// [ compound-operator select-core ]* [ ORDER BY ] [ LIMIT [ OFFSET ] ]
// ---------------------------------------------------------------------------

#[test]
fn select_every_clause_against_every_other() {
    let with = toggle(
        Piece::new(
            "with",
            r#"WITH "recent" AS (SELECT "id" FROM "posts" WHERE ("views" > #))"#,
            |q| {
                select::with(
                    "recent",
                    sqlite::select((
                        select::columns(quote("id")),
                        select::from(quote("posts")),
                        select::where_(quote("views").gt(arg(101i32))),
                    )),
                )
                .apply(q)
            },
        )
        .vals(&[101]),
    );

    let head = vec![Piece::new("", "SELECT", |_| {})];
    let distinct = toggle(Piece::new("distinct", "DISTINCT", |q| {
        select::distinct().apply(q)
    }));
    // A fixed one-column projection so every compound operand has matching arity
    // and every compound ORDER BY term names a result column of the first core.
    let columns = vec![Piece::new("", r#""name""#, |q| {
        select::columns(quote("name")).apply(q)
    })];
    let from = vec![Piece::new("", r#"FROM "users""#, |q| {
        select::from(quote("users")).apply(q)
    })];

    let mut join = vec![
        none(),
        Piece::new(
            "inner_join",
            r#"INNER JOIN "posts" ON ("posts"."user_id" = "users"."id")"#,
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
            r#"LEFT JOIN "posts" ON ("posts"."user_id" = "users"."id")"#,
            |q| {
                select::left_join(quote("posts"))
                    .on_eq(quote(("posts", "user_id")), quote(("users", "id")))
                    .apply(q)
            },
        ));
        // A CROSS JOIN takes a constraint in SQLite: it is an inner join that
        // additionally pins the join order.
        join.push(Piece::new(
            "cross_join",
            r#"CROSS JOIN "posts" ON ("posts"."user_id" = "users"."id")"#,
            |q| {
                select::cross_join(quote("posts"))
                    .on_eq(quote(("posts", "user_id")), quote(("users", "id")))
                    .apply(q)
            },
        ));
        // FULL JOIN needs SQLite 3.39+, which the linked engine satisfies.
        join.push(Piece::new(
            "full_join",
            r#"FULL JOIN "posts" USING ("id")"#,
            |q| select::full_join(quote("posts")).using(["id"]).apply(q),
        ));
    }

    let where_ = toggle(
        Piece::new("where", r#"WHERE ("age" >= #)"#, |q| {
            select::where_(quote("age").gte(arg(21i32))).apply(q)
        })
        .vals(&[21]),
    );
    let group_by = toggle(Piece::new("group_by", r#"GROUP BY "name""#, |q| {
        select::group_by(quote("name")).apply(q)
    }));
    // SQLite parses HAVING without GROUP BY, but the engine rejects it unless
    // the query aggregates ("HAVING clause on a non-aggregate query" — found by
    // this suite's first run). The projection here is a plain column, so in this
    // product HAVING is only a statement once GROUP BY makes it an aggregate;
    // the aggregate-without-GROUP-BY shape is grammar_select's to cover.
    let having = toggle(
        Piece::new("having", r#"HAVING (count(*) > #)"#, |q| {
            select::having(Expr::func("count", "*").gt(arg(3i32))).apply(q)
        })
        .vals(&[3])
        .requires(&["group_by"]),
    );
    // A named window nothing references is still a WINDOW clause per the
    // diagram; partitioning by the projected column keeps it resolvable.
    let window_ = toggle(Piece::new(
        "window",
        r#"WINDOW "w" AS (PARTITION BY "name")"#,
        |q| select::window("w", window::partition_by(quote("name"))).apply(q),
    ));

    fn second_core() -> sqlite::SelectQuery {
        sqlite::select((
            select::columns(quote("title")),
            select::from(quote("posts")),
        ))
    }
    let mut compound = vec![
        none(),
        Piece::new("union", r#"UNION SELECT "title" FROM "posts""#, |q| {
            select::union(second_core()).apply(q)
        }),
    ];
    if cfg!(feature = "exhaustive") {
        compound.push(Piece::new(
            "union_all",
            r#"UNION ALL SELECT "title" FROM "posts""#,
            |q| select::union_all(second_core()).apply(q),
        ));
        compound.push(Piece::new(
            "intersect",
            r#"INTERSECT SELECT "title" FROM "posts""#,
            |q| select::intersect(second_core()).apply(q),
        ));
        compound.push(Piece::new(
            "except",
            r#"EXCEPT SELECT "title" FROM "posts""#,
            |q| select::except(second_core()).apply(q),
        ));
    }

    // In a compound the ORDER BY term must name a result column of the first
    // core; `"name"` does in every combination, compound or not.
    let mut order_by = vec![
        none(),
        Piece::new("order_by", r#"ORDER BY "name" DESC"#, |q| {
            select::order_by(quote("name")).desc().apply(q)
        }),
    ];
    if cfg!(feature = "exhaustive") {
        // ordering-term: expr [ COLLATE name ] [ ASC | DESC ] [ NULLS … ].
        order_by.push(Piece::new(
            "order_by_decorated",
            r#"ORDER BY "name" COLLATE "NOCASE" DESC NULLS LAST"#,
            |q| {
                select::order_by(quote("name"))
                    .collate("NOCASE")
                    .desc()
                    .nulls_last()
                    .apply(q)
            },
        ));
    }

    let limit =
        toggle(Piece::new("limit", "LIMIT #", |q| select::limit(arg(10i32)).apply(q)).vals(&[10]));
    // `LIMIT expr [ OFFSET expr ]`: no production lets an offset stand alone.
    let offset = toggle(
        Piece::new("offset", "OFFSET #", |q| select::offset(arg(5i32)).apply(q))
            .vals(&[5])
            .requires(&["limit"]),
    );

    let cases = run(
        "sqlite SELECT",
        || sqlite::select(()),
        &[
            with, head, distinct, columns, from, join, where_, group_by, having, window_, compound,
            order_by, limit, offset,
        ],
    );
    assert!(cases >= 1152, "the cross product shrank: {cases}");
}

// ---------------------------------------------------------------------------
// INSERT — <https://www.sqlite.org/lang_insert.html>
//
// [ WITH … ] INSERT [ OR … ] INTO table [ (cols) ]
// ( VALUES … | select | DEFAULT VALUES ) [ upsert-clause ]* [ RETURNING … ]
// ---------------------------------------------------------------------------

#[test]
fn insert_every_clause_against_every_other() {
    let with = toggle(
        Piece::new(
            "with",
            r#"WITH "recent" AS (SELECT "id" FROM "posts" WHERE ("views" > #))"#,
            |q| {
                insert::with(
                    "recent",
                    sqlite::select((
                        select::columns(quote("id")),
                        select::from(quote("posts")),
                        select::where_(quote("views").gt(arg(101i32))),
                    )),
                )
                .apply(q)
            },
        )
        .vals(&[101]),
    );

    // `INSERT [ OR conflict-algorithm ]` — the head carries the whole choice.
    let head = vec![
        Piece::new("", "INSERT", |_| {}),
        Piece::new("or_rollback", "INSERT OR ROLLBACK", |q| {
            insert::or_rollback().apply(q)
        }),
        Piece::new("or_abort", "INSERT OR ABORT", |q| {
            insert::or_abort().apply(q)
        }),
        Piece::new("or_replace", "INSERT OR REPLACE", |q| {
            insert::or_replace().apply(q)
        }),
        Piece::new("or_fail", "INSERT OR FAIL", |q| insert::or_fail().apply(q)),
        Piece::new("or_ignore", "INSERT OR IGNORE", |q| {
            insert::or_ignore().apply(q)
        }),
    ];

    // The row source decides the target's column list too, so they are one
    // dimension: DEFAULT VALUES admits no column list, the others want one.
    let source = vec![
        Piece::new("values", r#"INTO "tags" ("name") VALUES (#)"#, |q| {
            insert::into(quote("tags")).columns(["name"]).apply(q);
            insert::values(arg(7i32)).apply(q);
        })
        .vals(&[7]),
        Piece::new(
            "values_rows",
            r#"INTO "tags" ("name") VALUES (#), (#)"#,
            |q| {
                insert::into(quote("tags")).columns(["name"]).apply(q);
                insert::values(arg(7i32)).apply(q);
                insert::values(arg(8i32)).apply(q);
            },
        )
        .vals(&[7, 8]),
        // The sub-select carries a WHERE because SQLite requires one between a
        // SELECT row source and an upsert clause, resolving the `ON` ambiguity.
        Piece::new(
            "select_source",
            r#"INTO "tags" ("name") SELECT "name" FROM "users" WHERE ("is_active" = #)"#,
            |q| {
                insert::into(quote("tags")).columns(["name"]).apply(q);
                insert::query(sqlite::select((
                    select::columns(quote("name")),
                    select::from(quote("users")),
                    select::where_(quote("is_active").eq(arg(1i32))),
                )))
                .apply(q);
            },
        )
        .vals(&[1]),
        Piece::new("default_values", r#"INTO "users" DEFAULT VALUES"#, |q| {
            insert::into(quote("users")).apply(q)
        }),
    ];

    // upsert-clause: the diagram hangs it off VALUES and select only, never off
    // DEFAULT VALUES — hence the conflict. Targets name `tags`, whose UNIQUE
    // (name) provides the inferred index.
    let upsert_conflicts: &[&str] = &["default_values"];
    let upsert = vec![
        none(),
        Piece::new(
            "do_nothing",
            r#"ON CONFLICT ("name") DO NOTHING"#,
            |q| {
                insert::on_conflict(quote("name")).do_nothing().apply(q);
            },
        )
        .conflicts(upsert_conflicts),
        Piece::new("do_nothing_any", "ON CONFLICT DO NOTHING", |q| {
            insert::on_conflict(()).do_nothing().apply(q);
        })
        .conflicts(upsert_conflicts),
        Piece::new(
            "do_update",
            r#"ON CONFLICT ("name") DO UPDATE SET "name" = excluded."name""#,
            |q| {
                insert::on_conflict(quote("name"))
                    .do_update(insert::set_excluded(["name"]))
                    .apply(q);
            },
        )
        .conflicts(upsert_conflicts),
        // The row filter qualifies its column: unqualified, it would be
        // ambiguous against EXCLUDED.
        Piece::new(
            "do_update_where",
            r#"ON CONFLICT ("name") DO UPDATE SET "name" = excluded."name" WHERE ("tags"."id" > #)"#,
            |q| {
                insert::on_conflict(quote("name"))
                    .do_update((
                        insert::set_excluded(["name"]),
                        insert::where_(quote(("tags", "id")).gt(arg(50i32))),
                    ))
                    .apply(q);
            },
        )
        .vals(&[50])
        .conflicts(upsert_conflicts),
        // Several upserts, tried in order; only the last may omit its target.
        Piece::new(
            "two_upserts",
            r#"ON CONFLICT ("name") DO UPDATE SET "name" = excluded."name" ON CONFLICT DO NOTHING"#,
            |q| {
                insert::on_conflict(quote("name"))
                    .do_update(insert::set_excluded(["name"]))
                    .apply(q);
                insert::on_conflict(()).do_nothing().apply(q);
            },
        )
        .conflicts(upsert_conflicts),
    ];

    // `id` exists on both `tags` and `users`, so RETURNING resolves under every
    // source.
    let returning = toggle(Piece::new("returning", r#"RETURNING "id""#, |q| {
        insert::returning(quote("id")).apply(q)
    }));

    let cases = run(
        "sqlite INSERT",
        || sqlite::insert(()),
        &[with, head, source, upsert, returning],
    );
    assert!(cases >= 300, "the cross product shrank: {cases}");
}

// ---------------------------------------------------------------------------
// UPDATE — <https://www.sqlite.org/lang_update.html>
//
// [ WITH … ] UPDATE [ OR … ] qualified-table-name SET … [ FROM … ]
// [ WHERE ] [ RETURNING ]
//
// The target is `tags`, whose UNIQUE (name) gives INDEXED BY a real index, and
// whose `name` column no other table here shares — so an unqualified `name`
// resolves under every alias and every FROM.
// ---------------------------------------------------------------------------

#[test]
fn update_every_clause_against_every_other() {
    let with = toggle(
        Piece::new(
            "with",
            r#"WITH "recent" AS (SELECT "id" FROM "posts" WHERE ("views" > #))"#,
            |q| {
                update::with(
                    "recent",
                    sqlite::select((
                        select::columns(quote("id")),
                        select::from(quote("posts")),
                        select::where_(quote("views").gt(arg(101i32))),
                    )),
                )
                .apply(q)
            },
        )
        .vals(&[101]),
    );

    let head = vec![
        Piece::new("", "UPDATE", |_| {}),
        Piece::new("or_rollback", "UPDATE OR ROLLBACK", |q| {
            update::or_rollback().apply(q)
        }),
        Piece::new("or_abort", "UPDATE OR ABORT", |q| {
            update::or_abort().apply(q)
        }),
        Piece::new("or_replace", "UPDATE OR REPLACE", |q| {
            update::or_replace().apply(q)
        }),
        Piece::new("or_fail", "UPDATE OR FAIL", |q| update::or_fail().apply(q)),
        Piece::new("or_ignore", "UPDATE OR IGNORE", |q| {
            update::or_ignore().apply(q)
        }),
    ];

    // qualified-table-name: name [ AS alias ] [ INDEXED BY … | NOT INDEXED ].
    let target = vec![
        Piece::new("target", r#""tags""#, |q| {
            update::table(quote("tags")).apply(q)
        }),
        Piece::new("target_alias", r#""tags" AS "t""#, |q| {
            update::table(quote("tags")).as_("t").apply(q)
        }),
        Piece::new(
            "target_indexed",
            r#""tags" INDEXED BY "sqlite_autoindex_tags_1""#,
            |q| {
                update::table(quote("tags"))
                    .indexed_by("sqlite_autoindex_tags_1")
                    .apply(q)
            },
        ),
        Piece::new("target_not_indexed", r#""tags" NOT INDEXED"#, |q| {
            update::table(quote("tags")).not_indexed().apply(q)
        }),
    ];

    let set = vec![
        Piece::new("", r#"SET "name" = #"#, |q| {
            update::set_col("name").to_arg(7i32).apply(q)
        })
        .vals(&[7]),
    ];

    // `posts` shares no column name with `tags` except `id`, and nothing below
    // says a bare `id`, so the extra from-item introduces no ambiguity.
    let from = toggle(Piece::new("from", r#"FROM "posts" AS "p""#, |q| {
        update::from(quote("posts")).as_("p").apply(q)
    }));

    let where_ = toggle(
        Piece::new("where", r#"WHERE ("name" > #)"#, |q| {
            update::where_(quote("name").gt(arg(21i32))).apply(q)
        })
        .vals(&[21]),
    );

    let returning = toggle(Piece::new("returning", r#"RETURNING "name""#, |q| {
        update::returning(quote("name")).apply(q)
    }));

    let cases = run(
        "sqlite UPDATE",
        || sqlite::update(()),
        &[with, head, target, set, from, where_, returning],
    );
    assert!(cases >= 384, "the cross product shrank: {cases}");
}

// ---------------------------------------------------------------------------
// DELETE — <https://www.sqlite.org/lang_delete.html>
//
// [ WITH … ] DELETE FROM qualified-table-name [ WHERE ] [ RETURNING ]
//
// No OR prefix (only INSERT and UPDATE can violate a constraint), no USING, no
// joins — the shortest statement, so the whole product is small.
// ---------------------------------------------------------------------------

#[test]
fn delete_every_clause_against_every_other() {
    let with = toggle(
        Piece::new(
            "with",
            r#"WITH "recent" AS (SELECT "id" FROM "posts" WHERE ("views" > #))"#,
            |q| {
                delete::with(
                    "recent",
                    sqlite::select((
                        select::columns(quote("id")),
                        select::from(quote("posts")),
                        select::where_(quote("views").gt(arg(101i32))),
                    )),
                )
                .apply(q)
            },
        )
        .vals(&[101]),
    );

    let head = vec![Piece::new("", "DELETE", |_| {})];

    let target = vec![
        Piece::new("target", r#"FROM "tags""#, |q| {
            delete::from(quote("tags")).apply(q)
        }),
        Piece::new("target_alias", r#"FROM "tags" AS "t""#, |q| {
            delete::from(quote("tags")).as_("t").apply(q)
        }),
        Piece::new(
            "target_indexed",
            r#"FROM "tags" INDEXED BY "sqlite_autoindex_tags_1""#,
            |q| {
                delete::from(quote("tags"))
                    .indexed_by("sqlite_autoindex_tags_1")
                    .apply(q)
            },
        ),
        Piece::new("target_not_indexed", r#"FROM "tags" NOT INDEXED"#, |q| {
            delete::from(quote("tags")).not_indexed().apply(q)
        }),
    ];

    let where_ = toggle(
        Piece::new("where", r#"WHERE ("name" > #)"#, |q| {
            delete::where_(quote("name").gt(arg(21i32))).apply(q)
        })
        .vals(&[21]),
    );

    let returning = toggle(Piece::new("returning", r#"RETURNING "name""#, |q| {
        delete::returning(quote("name")).apply(q)
    }));

    let cases = run(
        "sqlite DELETE",
        || sqlite::delete(()),
        &[with, head, target, where_, returning],
    );
    assert!(cases >= 32, "the cross product shrank: {cases}");
}

/// The twin of the join-mods guard: the extra from-items of `from_also` are
/// second and later entries of the list the leading `FROM` item opens, so with
/// no leading item they used to be dropped silently — valid SQL, the caller's
/// item simply gone. Now `build()` refuses. DELETE has no from-item
/// list at all — no `using`, no `from_also` — so SELECT and UPDATE are the
/// whole surface.
#[test]
fn extra_from_items_without_a_leading_item_are_a_build_error() {
    let q = sqlite::select((
        select::columns(quote("id")),
        select::from_also(quote("users")),
    ));
    let err = q.build().unwrap_err();
    // The substring names the SQL concept (the missing leading FROM item), not
    // the message wording.
    assert!(
        matches!(&err, sqlite::Error::Incomplete(what) if what.contains("FROM")),
        "got: {err}"
    );

    let q = sqlite::update((
        update::table(quote("posts")),
        update::set_col("views").to(arg(1i32)),
        update::from_also(quote("users")),
    ));
    let err = q.build().unwrap_err();
    assert!(
        matches!(&err, sqlite::Error::Incomplete(what) if what.contains("FROM")),
        "got: {err}"
    );
}
