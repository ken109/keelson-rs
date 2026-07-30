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

    if live::available().contains(&Dialect::Mysql) {
        live::assert_valid(Dialect::Mysql, &sql);
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
    assert_eq!(
        normalize(&sql),
        normalize(expected),
        "SQL is not what was expected (both tiers skipped: {construct}; {reason})"
    );
    args
}
