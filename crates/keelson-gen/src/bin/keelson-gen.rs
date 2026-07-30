//! The thin CLI over [`keelson_gen::run`].
//!
//! ```text
//! keelson-gen [--config keelson.toml] [--url <connection>] [--out <dir>]
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

use std::process::ExitCode;

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("keelson-gen: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<(), String> {
    let mut config_path = "keelson.toml".to_owned();
    let mut url = None;
    let mut out = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut take = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--config" => config_path = take("--config")?,
            "--url" => url = Some(take("--url")?),
            "--out" => out = Some(take("--out")?),
            "--help" | "-h" => {
                println!(
                    "keelson-gen [--config keelson.toml] [--url <connection>] [--out <dir>]\n\
                     Introspects the configured database and (re)generates the model files."
                );
                return Ok(());
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

    // The models. A config that carries only a `[queries]` section is
    // generating Layer 4 alone and needs no `out`; otherwise a missing `out`
    // is the same error it has always been.
    if config.out.is_some() || config.queries.is_none() {
        for path in keelson_gen::run(&config).map_err(|e| e.to_string())? {
            println!("wrote {}", path.display());
        }
    }
    // Layer 4: one module per hand-written `.sql` file (see
    // `keelson_gen::queries`).
    if config.queries.is_some() {
        for path in keelson_gen::queries::run(&config).map_err(|e| e.to_string())? {
            println!("wrote {}", path.display());
        }
    }
    Ok(())
}
