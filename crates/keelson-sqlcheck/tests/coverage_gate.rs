//! Meta-tests for Tier D: prove the gate *fails* when a declared construct is
//! missing from the recording, and passes it when it appears.
//!
//! A coverage gate that cannot fail is a green light wired to nothing. These
//! tests feed the analyzer a hand-built corpus and assert on the failure list
//! itself, so the wiring from "absent in recording" to "named in the report"
//! is what is tested — not any particular real coverage number, which belongs
//! to the real record-then-gate run.

use keelson_sqlcheck::Dialect;
use keelson_sqlcheck::coverage::{Config, analyze};

fn corpus(records: &[(Dialect, &str)]) -> Vec<(Dialect, String)> {
    records
        .iter()
        .map(|(d, sql)| (*d, (*sql).to_string()))
        .collect()
}

/// The gate must fail a recording that never rendered a declared construct,
/// and must say which construct by id and API.
#[test]
fn a_recording_missing_a_declared_construct_fails_the_gate_naming_it() {
    let config = Config::embedded().expect("checked-in manifests are valid");
    // A tiny corpus that exercises a handful of constructs — and, crucially,
    // no FULL JOIN anywhere.
    let records = corpus(&[
        (Dialect::Psql, r#"SELECT "id" FROM users WHERE "age" >= $1"#),
        (Dialect::Mysql, "SELECT `id` FROM `users` LIMIT 10"),
        (Dialect::Sqlite, r#"SELECT "id" FROM users LIMIT ?1"#),
    ]);

    let report = analyze(&records, &config, 0);
    assert!(
        !report.passed(),
        "a near-empty recording must fail the gate"
    );

    let psql = &report.outcomes[0];
    assert_eq!(psql.dialect, Dialect::Psql);
    let missing_full_join = psql
        .unexercised
        .iter()
        .find(|e| e.id == "join.full")
        .expect("the gate names join.full as unexercised");
    assert!(
        missing_full_join.api.contains("full_join"),
        "the failure names the API that would exercise it: {}",
        missing_full_join.api
    );
    // The report renders the failure for a human, id and API both.
    let rendered = report.render();
    assert!(rendered.contains("UNEXERCISED"));
    assert!(rendered.contains("join.full"));
    assert!(rendered.contains("coverage gate: FAIL"));
}

/// What the corpus does exercise must not be reported missing — in the tree
/// tier and the token tier both.
#[test]
fn exercised_constructs_are_not_reported_missing() {
    let config = Config::embedded().expect("checked-in manifests are valid");
    let records = corpus(&[
        (
            Dialect::Psql,
            r#"SELECT "id" FROM users FULL JOIN posts ON ("posts"."user_id" = "users"."id")"#,
        ),
        (Dialect::Mysql, "SELECT `id` FROM `users` WHERE `id` <=> ?"),
        (
            Dialect::Sqlite,
            r#"SELECT "id" FROM users WHERE "name" GLOB 'a*'"#,
        ),
    ]);

    let report = analyze(&records, &config, 0);
    let by_dialect = |d: Dialect| {
        report
            .outcomes
            .iter()
            .find(|o| o.dialect == d)
            .expect("every dialect has an outcome")
    };
    for (dialect, id) in [
        (Dialect::Psql, "join.full"),
        (Dialect::Psql, "join.on"),
        (Dialect::Mysql, "op.null_safe_eq"),
        (Dialect::Sqlite, "op.glob"),
    ] {
        let outcome = by_dialect(dialect);
        assert!(
            outcome.exercised.contains(id),
            "{id} was rendered, so {} must count it exercised",
            dialect.name()
        );
        assert!(
            outcome.unexercised.iter().all(|e| e.id != id),
            "{id} must not be in {}'s failure list",
            dialect.name()
        );
    }
}

/// A recorded psql statement the parser no longer accepts must fail the gate
/// rather than silently shrink the corpus.
#[test]
fn a_stale_unparseable_recording_fails_the_gate() {
    let config = Config::embedded().expect("checked-in manifests are valid");
    let records = corpus(&[(Dialect::Psql, "SELECT * FORM users")]);
    let report = analyze(&records, &config, 0);
    assert!(!report.passed());
    assert_eq!(report.outcomes[0].parse_failures.len(), 1);
}

/// Malformed recording lines (counted by the reader) also fail the gate: a
/// corrupt corpus understates coverage, which is the one direction the gate
/// exists to refuse.
#[test]
fn malformed_recording_lines_fail_the_gate() {
    let config = Config::embedded().expect("checked-in manifests are valid");
    let report = analyze(&[], &config, 3);
    assert!(!report.passed());
    assert!(report.render().contains("3 malformed recording lines"));
}
