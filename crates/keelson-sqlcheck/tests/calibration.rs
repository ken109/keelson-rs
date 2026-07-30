//! Calibrates the judge against SQL known to be valid.
//!
//! The imported bob fixtures are 73 statements that a real parser accepted on the
//! Go side, so every one of them *should* pass here. Where one does not, the
//! limitation is in our judge, not in the SQL — and knowing exactly where those
//! limits are is what stops us trusting a check that cannot deliver.
//!
//! This is the check that keeps the two oracles honest about each other.

use keelson_sqlcheck::{Dialect, check};

/// MySQL statements our judge wrongly rejects.
///
/// `sqlparser` is a generic parser with a MySQL dialect, not MySQL's own grammar.
/// It does not implement multi-table `UPDATE a, b SET ...`, which is valid MySQL.
/// Listed by the fixture case name so the exception is scoped to a known case
/// rather than silencing a whole class of failure.
const KNOWN_FALSE_NEGATIVES: &[(&str, &str)] = &[(
    "mysql",
    "update multiple tables",
)];

fn is_known(dialect: &str, name: &str) -> bool {
    KNOWN_FALSE_NEGATIVES
        .iter()
        .any(|(d, n)| *d == dialect && *n == name)
}

#[test]
fn every_imported_case_parses() {
    let mut unexpected = Vec::new();
    let mut known_hit = Vec::new();
    let mut checked = 0;

    for case in keelson_golden::all() {
        let Some(dialect) = Dialect::from_name(&case.dialect) else {
            continue; // dialect-agnostic expression cases have no grammar to check
        };
        if case.generated_sql.trim().is_empty() {
            continue; // error-path cases produced no SQL
        }

        checked += 1;
        match check(dialect, &case.generated_sql) {
            Ok(()) => {
                if is_known(&case.dialect, &case.name) {
                    known_hit.push(case.name.clone());
                }
            }
            Err(e) => {
                if is_known(&case.dialect, &case.name) {
                    continue;
                }
                unexpected.push(format!(
                    "{} {:?}\n    parser: {e}\n    sql:    {}",
                    case.dialect, case.name, case.generated_sql
                ));
            }
        }
    }

    assert!(checked >= 70, "expected to check ~73 statements, checked {checked}");

    assert!(
        unexpected.is_empty(),
        "the judge rejected {} statement(s) that a real parser accepted on the Go side.\n\
         Either our judge is weaker than believed (add to KNOWN_FALSE_NEGATIVES with a \
         reason) or the fixture is wrong:\n\n{}",
        unexpected.len(),
        unexpected.join("\n\n"),
    );

    assert!(
        known_hit.is_empty(),
        "these cases are listed as false negatives but now parse cleanly — the judge \
         improved, so drop them from KNOWN_FALSE_NEGATIVES: {known_hit:?}",
    );
}

/// Records which dialects can be trusted, so a test author knows what a pass buys.
#[test]
fn trust_levels_are_what_we_measured() {
    // psql and sqlite accepted all of their imported statements and reject
    // malformed input, so a pass there is real evidence.
    for d in [Dialect::Psql, Dialect::Sqlite] {
        assert!(d.is_authoritative(), "{d:?} should be authoritative");
    }
    // MySQL fails in both directions: it accepts PostgreSQL-only `DISTINCT ON`
    // and rejects valid multi-table UPDATE. Advisory only.
    assert!(!Dialect::Mysql.is_authoritative());
}
