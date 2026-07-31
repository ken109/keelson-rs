//! Shared scaffolding for keelson's examples: a throwaway SQLite database,
//! the generated models and queries, and the hand-written hooks they delegate
//! to.
//!
//! Nothing here is part of keelson. It is the small amount of application code
//! the examples need in order to have something to run against, kept out of
//! the example files so each of those stays about one topic.

/// The generated model layer, written by `keelson-gen` from `schema.sql`.
///
/// Committed on purpose: generated code you can open, diff and step through is
/// the whole point of a CLI generator over a proc macro. Re-run the generator
/// after a migration; `tests/generated_is_fresh.rs` fails if you forget.
// prettyplease formatted these files, and `cargo fmt` must not fight it --
// the freshness test compares bytes.
#[rustfmt::skip]
pub mod models;

/// The generated Layer 4 modules, one per `.sql` file in `queries/`.
#[rustfmt::skip]
pub mod queries;

pub mod hooks;

use std::path::{Path, PathBuf};

use keelson::exec::{ExecError, Executor as _, Statement};
use keelson::sqlx::sqlite::Pool;

/// The DDL, compiled in so an example needs no working directory.
const SCHEMA: &str = include_str!("../schema.sql");

/// A SQLite database in a temporary file, removed when this value is dropped.
///
/// A file rather than `sqlite::memory:` because a pool opens several
/// connections and each would otherwise get a private, empty in-memory
/// database of its own.
#[derive(Debug)]
pub struct Sandbox {
    /// The pool every example passes to `fetch_all`, `execute` and friends.
    pub db: Pool,
    path: PathBuf,
}

impl Sandbox {
    /// A database with the schema applied and no rows in it.
    pub async fn empty() -> Result<Sandbox, ExecError> {
        let path = temp_path();
        let db = Pool::connect(&format!("sqlite://{}", path.display())).await?;
        for statement in SCHEMA.split(';').filter(|s| !s.trim().is_empty()) {
            db.execute(Statement::new(statement.to_owned(), vec![]))
                .await?;
        }
        Ok(Sandbox { db, path })
    }

    /// A database with the schema applied and a handful of rows in it: three
    /// users, four posts, three comments, three tags.
    ///
    /// Keys are fixed rather than auto-assigned so that examples can assert on
    /// them.
    pub async fn seeded() -> Result<Sandbox, ExecError> {
        let sandbox = Sandbox::empty().await?;
        seed(&sandbox.db).await?;
        Ok(sandbox)
    }

    /// The database file, for an example that wants to say where it is.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Best effort: an example that panicked mid-run still leaves the file
        // in the temp directory, where the operating system will get to it.
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("db-wal"));
        let _ = std::fs::remove_file(self.path.with_extension("db-shm"));
    }
}

fn temp_path() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "keelson-example-{}-{}.db",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// The seed rows, written with the Layer 1 builder -- which is also the
/// shortest possible demonstration of a multi-row `INSERT`.
async fn seed(db: &Pool) -> Result<(), ExecError> {
    use keelson::exec::Execute as _;
    use keelson::sqlite::{arg, insert, quote};

    keelson::sqlite::insert((
        insert::into(quote("users")).columns(["id", "name", "email", "age"]),
        insert::values((arg(1), arg("Ada"), arg("ada@example.com"), arg(36))),
        insert::values((arg(2), arg("Grace"), arg("grace@example.com"), arg(45))),
        // No email, and under age: the row that makes `Option` and `WHERE`
        // interesting in the examples that filter.
        insert::values((arg(3), arg("Kid"), keelson::sqlite::raw("NULL"), arg(12))),
    ))
    .execute(db)
    .await?;

    keelson::sqlite::insert((
        insert::into(quote("posts")).columns(["id", "user_id", "title", "status", "views"]),
        insert::values((arg(1), arg(1), arg("Hello"), arg("published"), arg(120))),
        insert::values((arg(2), arg(1), arg("Drafts"), arg("draft"), arg(3))),
        insert::values((arg(3), arg(2), arg("Compilers"), arg("published"), arg(980))),
        insert::values((arg(4), arg(2), arg("Bugs"), arg("published"), arg(7))),
    ))
    .execute(db)
    .await?;

    keelson::sqlite::insert((
        insert::into(quote("comments")).columns(["id", "post_id", "user_id", "body"]),
        insert::values((arg(1), arg(1), arg(2), arg("Nice one"))),
        insert::values((arg(2), arg(1), arg(3), arg("+1"))),
        // An anonymous comment: the nullable foreign key, exercised.
        insert::values((
            arg(3),
            arg(3),
            keelson::sqlite::raw("NULL"),
            arg("Posted by nobody"),
        )),
    ))
    .execute(db)
    .await?;

    keelson::sqlite::insert((
        insert::into(quote("tags")).columns(["id", "name"]),
        insert::values((arg(1), arg("rust"))),
        insert::values((arg(2), arg("sql"))),
        insert::values((arg(3), arg("perf"))),
    ))
    .execute(db)
    .await?;

    keelson::sqlite::insert((
        insert::into(quote("post_tags")).columns(["post_id", "tag_id"]),
        insert::values((arg(1), arg(1))),
        insert::values((arg(1), arg(2))),
        insert::values((arg(3), arg(1))),
        insert::values((arg(3), arg(3))),
    ))
    .execute(db)
    .await?;

    Ok(())
}

/// Print a built statement the way every Layer 1 example does: the SQL, then
/// the arguments that will be bound to its placeholders.
pub fn show(title: &str, sql: &str, args: &[keelson::Value]) {
    println!("── {title}\n{sql}");
    if !args.is_empty() {
        println!("   args: {args:?}");
    }
    println!();
}
