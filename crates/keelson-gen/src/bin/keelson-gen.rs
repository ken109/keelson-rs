//! The thin CLI over [`keelson_gen::run`].
//!
//! ```text
//! keelson-gen [--config keelson.toml] [--url <connection>] [--out <dir>] [--check]
//! ```
//!
//! `--url` and `--out` override the config file's values, so the connection
//! string can stay out of committed configuration.
//!
//! One invocation generates whatever the config asks for: the models, and —
//! when a `[queries]` section is present — one module per hand-written `.sql`
//! file as well (`keelson_gen::queries`). The two outputs go to different
//! directories and never share a file, so a config with only `[queries]`
//! generates Layer 4 alone.
//!
//! `--check` is the same run with the writing removed: it renders, compares
//! against what is committed, prints what differs and exits non-zero. That is
//! the *migrate → regenerate → compile* loop made enforceable — committed
//! generated code that no longer matches its schema still compiles, so
//! `cargo build` cannot be the thing that notices.

use std::process::ExitCode;

use keelson_gen::Drift;

/// `--check` found drift. Distinct from `FAILURE` on purpose: a caller that
/// wants to tell "the tree is stale" apart from "the generator could not run"
/// (a bad connection string, an unmapped type) can, and a caller that does not
/// care sees a non-zero exit either way.
const DRIFT: u8 = 2;

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("keelson-gen: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<ExitCode, String> {
    let mut config_path = "keelson.toml".to_owned();
    let mut url = None;
    let mut out = None;
    let mut check = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut take = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--config" => config_path = take("--config")?,
            "--url" => url = Some(take("--url")?),
            "--out" => out = Some(take("--out")?),
            "--check" => check = true,
            "--version" | "-V" => {
                println!("keelson-gen {}", env!("CARGO_PKG_VERSION"));
                return Ok(ExitCode::SUCCESS);
            }
            "--help" | "-h" => {
                println!(
                    "keelson-gen [--config keelson.toml] [--url <connection>] [--out <dir>] [--check]\n\
                     \n\
                     Introspects the configured database and (re)generates the model files.\n\
                     \n\
                     Options:\n\
                     \x20 --config <path>  the generator configuration (default: keelson.toml)\n\
                     \x20 --url <conn>     connection string, overriding the config's\n\
                     \x20 --out <dir>      model output directory, overriding the config's\n\
                     \x20 --check          write nothing; report files that differ from what\n\
                     \x20                  would be generated, and exit 2 if any do\n\
                     \x20 -V, --version    print the version\n\
                     \x20 -h, --help       print this"
                );
                return Ok(ExitCode::SUCCESS);
            }
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }

    let mut config =
        keelson_gen::Config::load(&config_path).map_err(|e| format!("{config_path}: {e}"))?;
    if url.is_some() {
        config.url = url;
    }
    if out.is_some() {
        config.out = out;
    }

    // A config that carries only a `[queries]` section is generating Layer 4
    // alone and needs no `out`; otherwise a missing `out` is the same error it
    // has always been.
    let wants_models = config.out.is_some() || config.queries.is_none();
    let wants_queries = config.queries.is_some();

    if check {
        let mut drift = Vec::new();
        if wants_models {
            drift.extend(keelson_gen::check(&config).map_err(|e| e.to_string())?);
        }
        if wants_queries {
            drift.extend(keelson_gen::queries::check(&config).map_err(|e| e.to_string())?);
        }
        // Both entry points check the schema snapshot, because both are
        // correct to call on their own. Said once here.
        drift.sort_by(|a, b| a.path().cmp(b.path()));
        drift.dedup();
        return Ok(report(&drift));
    }

    let mut written = Vec::new();
    if wants_models {
        written.extend(keelson_gen::run(&config).map_err(|e| e.to_string())?);
    }
    // Layer 4: one module per hand-written `.sql` file (see
    // `keelson_gen::queries`).
    if wants_queries {
        written.extend(keelson_gen::queries::run(&config).map_err(|e| e.to_string())?);
    }
    // Same reason as the `--check` dedup above: the snapshot is refreshed by
    // whichever entry point ran, and both may have.
    let mut seen = std::collections::HashSet::new();
    for path in written {
        if seen.insert(path.clone()) {
            println!("wrote {}", path.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Print what `--check` found. The drift goes to stderr and the verdict to
/// stdout, so a script can capture one without the other.
fn report(drift: &[Drift]) -> ExitCode {
    if drift.is_empty() {
        println!("up to date");
        return ExitCode::SUCCESS;
    }
    for d in drift {
        eprintln!("{d}");
    }
    eprintln!(
        "\n{} file(s) differ from what the schema and the .sql files generate.\n\
         Re-run keelson-gen without --check and commit the result.",
        drift.len()
    );
    ExitCode::from(DRIFT)
}
