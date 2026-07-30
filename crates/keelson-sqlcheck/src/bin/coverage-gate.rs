//! Tier D's gate: replay a recording, measure it against the manifests, fail
//! on any declared-but-unexercised construct.
//!
//! ```text
//! KEELSON_SQLCHECK_RECORD=target/sqlcheck-record cargo test --workspace
//! cargo run -p keelson-sqlcheck --bin coverage-gate -- target/sqlcheck-record
//! ```
//!
//! Exit status 0 means every construct each dialect declares was exercised by
//! the recorded run; 1 means the report names what was not. The manifests are
//! compiled in from `crates/keelson-sqlcheck/coverage/`.

use std::path::Path;
use std::process::ExitCode;

use keelson_sqlcheck::coverage::{Config, analyze};
use keelson_sqlcheck::record;

fn main() -> ExitCode {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: coverage-gate <record-dir>");
        eprintln!(
            "record with: {}=<record-dir> cargo test --workspace",
            record::ENV_VAR
        );
        return ExitCode::FAILURE;
    };

    let config = match Config::embedded() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("checked-in manifest is invalid: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (records, malformed) = match record::read_dir(Path::new(&dir)) {
        Ok(read) => read,
        Err(e) => {
            eprintln!("cannot read recording {dir}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if records.is_empty() {
        eprintln!(
            "recording {dir} holds no records — run the tests with {}={dir} first",
            record::ENV_VAR
        );
        return ExitCode::FAILURE;
    }

    let report = analyze(&records, &config, malformed);
    print!("{}", report.render());
    if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
