//! The four assertions the MySQL grammar walk uses, and why there are four.
//!
//! [`check`] is [`assert_sql`] and is what every case should use: it runs the
//! `sqlparser` grammar tier, a real MySQL 8.4 when one is compiled in
//! (`--features live-docker`), and the whitespace-normalised comparison against the
//! string written in the test.
//!
//! The other three exist because **for MySQL the two tiers disagree, in both
//! directions**, and neither disagreement should be settled by weakening the SQL.
//!
//! * [`check_without_grammar`] drops the grammar tier for a construct `sqlparser`
//!   rejects and the manual and the server both accept. `sqlparser` is a generic
//!   parser wearing a MySQL hat, not MySQL's grammar; each call names the construct
//!   so the list of false negatives is readable from the tests. It *asserts* the
//!   disagreement is still real, so a stricter parser turns these back into
//!   ordinary cases rather than leaving a silent hole.
//! * [`check_without_engine`] drops the engine tier for SQL that is syntactically
//!   right and semantically impossible against the shared schema — in practice only
//!   `PARTITION`, because MySQL cannot partition a table that participates in a
//!   foreign key and every table in `tests/schema/mysql.sql` does.
//! * [`check_shape_only`] is the unhappy intersection: a construct `sqlparser`
//!   rejects *and* the shared schema cannot satisfy, so only the string comparison
//!   is left. Three cases need it, all of them `PARTITION` in a statement whose
//!   `PARTITION` `sqlparser` also cannot parse. Their expected strings come from the
//!   manual alone, and each one names the two reasons.
//!
//! All of them still assert the intended string, which is the part that says "this
//! is the SQL we meant" and the part no parser can answer.
//!
//! **A run with no judge announces itself.** On a plain `cargo test` there is no
//! engine, so a [`check_without_grammar`] case has nothing vouching for its SQL at
//! all, and a [`check_shape_only`] case never does on any build. Both write a
//! `SKIPPED (no judge)` line to stderr — past libtest's capture, so it is visible
//! without `--nocapture` — naming the case's file and line, instead of passing as
//! silently as a fully judged one. [`check_without_engine`] stays quiet: its
//! grammar tier does run.
//!
//! **Where the expected strings come from.** Each is derived from the statement's
//! production in the MySQL 8.4 reference manual — cited in the test where the shape
//! is not obvious — or from bob's rendering of the same construct where bob has one,
//! plus the rendering rules `keelson_core::clause` documents: a clause writes its own
//! keyword, an absent clause writes nothing at all, and every operator from the chain
//! parenthesises its own result exactly once. None was produced by running the
//! builder and pasting the output.
//!
//! **Every table and column named is in `tests/schema/mysql.sql`.** That is what lets
//! the engine tier resolve names, which is where the sharp failures are.

#![allow(dead_code)]

use keelson_mysql::{Query, Value};
use keelson_sqlcheck::{Dialect, assert_sql, assert_valid, live, normalize};

/// Whether this build reaches a real MySQL.
fn engine_available() -> bool {
    live::available().contains(&Dialect::Mysql)
}

/// Announce visibly that a case ran with no judge vouching for its SQL.
///
/// The weakened assertions below drop the `sqlparser` tier on purpose, which
/// leaves the engine as the only judge — and on a plain `cargo test` there is no
/// engine. `eprintln!` is captured and discarded for a passing test, so a message
/// through it is silence in exactly the situation this exists for; writing to the
/// stderr handle directly bypasses libtest's capture. `#[track_caller]` all the
/// way down makes the message name the test's own file and line.
#[track_caller]
fn announce_unjudged(construct: &str, detail: &str) {
    use std::io::Write as _;
    let caller = std::panic::Location::caller();
    // One pre-formatted write_all rather than writeln!'s piecewise writes, so
    // parallel tests cannot interleave inside a message.
    let line = format!(
        "SKIPPED (no judge) {caller}: {construct} — {detail}; only the string \
         comparison ran.\n"
    );
    let _ = std::io::stderr().write_all(line.as_bytes());
}

/// Build, then run every check this build can: grammar, engine, and intent.
#[track_caller]
pub(crate) fn check(q: &impl Query, expected: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query should build");
    assert_sql(Dialect::Mysql, &sql, expected);
    args
}

/// [`check`] without the grammar tier, for a construct `sqlparser` gets wrong.
///
/// `construct` names what it cannot parse; it appears in the panic message so a
/// failure here is never mistaken for the skipped check.
#[track_caller]
pub(crate) fn check_without_grammar(q: &impl Query, expected: &str, construct: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query should build");

    // Assert the disagreement is still real. If sqlparser learns the construct, this
    // fires and the case should move back to `check`.
    assert!(
        keelson_sqlcheck::check_mysql(&sql).is_err(),
        "sqlparser now accepts {construct} — move this case to `check`\n  sql: {sql}"
    );

    // Still a judged case — string intent always, engine when compiled in — so
    // its SQL belongs in Tier D's recording, which `check_mysql` only feeds on
    // success. No-op unless KEELSON_SQLCHECK_RECORD is set.
    keelson_sqlcheck::record(Dialect::Mysql, &sql);

    if engine_available() {
        live::assert_valid(Dialect::Mysql, &sql);
    } else {
        // With the grammar tier dropped, the engine was the only judge — and this
        // build has none. Say so out loud instead of passing as if verified.
        announce_unjudged(
            construct,
            "sqlparser cannot parse this valid MySQL and no engine is compiled in, \
             so nothing has vouched for the SQL; re-run with `--features live-docker` \
             to have a real MySQL 8.4 judge it",
        );
    }
    assert_eq!(
        normalize(&sql),
        normalize(expected),
        "SQL is not what was expected (grammar tier skipped: {construct})"
    );
    args
}

/// [`check`] without the engine tier, for SQL the shared schema cannot satisfy.
///
/// `reason` says why the server cannot be asked.
#[track_caller]
pub(crate) fn check_without_engine(q: &impl Query, expected: &str, reason: &str) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query should build");
    assert_valid(Dialect::Mysql, &sql);
    assert_eq!(
        normalize(&sql),
        normalize(expected),
        "SQL is not what was expected (engine tier skipped: {reason})"
    );
    args
}

/// The string comparison alone, for the cases where neither judge can be asked.
///
/// `construct` names what `sqlparser` cannot parse and `reason` why the server
/// cannot be asked either. The grammar half of the disagreement is still asserted,
/// so this weakens to [`check_without_engine`] the moment `sqlparser` improves.
///
/// **This is the weakest assertion in the file.** Use it only when the alternative
/// is not testing the construct at all, and derive `expected` from the manual.
#[track_caller]
pub(crate) fn check_shape_only(
    q: &impl Query,
    expected: &str,
    construct: &str,
    reason: &str,
) -> Vec<Value> {
    let (sql, args) = q.build().expect("the query should build");
    assert!(
        keelson_sqlcheck::check_mysql(&sql).is_err(),
        "sqlparser now accepts {construct} — move this case to `check_without_engine`\n  sql: {sql}"
    );
    // The string comparison still judges this case, so its SQL belongs in
    // Tier D's recording (no-op unless KEELSON_SQLCHECK_RECORD is set).
    keelson_sqlcheck::record(Dialect::Mysql, &sql);
    // No judge can be asked on *any* build — sqlparser cannot parse the construct
    // and the shared schema cannot satisfy it — so this announces on every run,
    // not only when the engine is missing. A silent pass here would read as
    // verified SQL, which it is not.
    announce_unjudged(
        construct,
        &format!("sqlparser cannot parse it and the engine cannot be asked ({reason})"),
    );
    assert_eq!(
        normalize(&sql),
        normalize(expected),
        "SQL is not what was expected (both tiers skipped: {construct}; {reason})"
    );
    args
}
