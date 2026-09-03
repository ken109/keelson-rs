//! The suite every execution backend has to pass, written once.
//!
//! A backend's job is one pair of functions — bind a [`Value`] into the
//! driver's parameter, decode the driver's column back into a `Value` — and
//! `docs/type-mappings.md` says what that pair must do for each mapped type.
//! This is that table in executable form: bind → `INSERT` → `SELECT` back →
//! compare with the type's own semantic equality, over every mapped type and
//! the edges of each.
//!
//! # Why it is here rather than in a backend's tests
//!
//! It used to live in `keelson-sqlx/tests/roundtrip.rs`, shared across that
//! crate's three engines and nowhere else. When a second PostgreSQL backend
//! arrived, it wrote its own — seven types with one value apiece, against
//! twelve types with some forty values here. Nothing said the newer backend
//! was thinner; the two files simply had no relationship, and the gap was
//! invisible unless you counted `#[tokio::test]`s in both. `f64`, `bytea`,
//! `date`, `time`, `timestamp` and `numeric` had no round-trip coverage on
//! that backend at all.
//!
//! Taking `&dyn Executor` is what makes one suite serve all of them: a
//! backend is exactly what that trait says it is, so a conformance test needs
//! nothing else. A new backend gets the whole suite by calling one function,
//! and cannot be thinner than the others by accident.
//!
//! # Using it
//!
//! ```ignore
//! #[tokio::test]
//! async fn every_mapped_type_round_trips() {
//!     let db = pool().await;
//!     db.execute(Statement::new(conformance::ddl(db.family()), vec![]))
//!         .await
//!         .unwrap();
//!     conformance::every_mapped_type_round_trips(&db).await;
//! }
//! ```
//!
//! The table is [`TABLE`], and its columns are only the ones all three
//! engines have. A backend's *own* tests — pinned storage forms, engine
//! widening rules, arrays, isolation levels — stay in that backend, against
//! its own table; this suite is the floor, not the ceiling.
//!
//! # The one type a backend may decline
//!
//! keelson-tokio-postgres carries no `Decimal`: `rust_decimal`'s PostgreSQL
//! support is a feature *of rust_decimal*, and enabling it would add
//! tokio-postgres to the dependency graph of every crate here that touches
//! `rust_decimal` (its crate docs say so). It calls
//! [`every_mapped_type_round_trips_except`] and names [`Mapped::Decimal`].
//!
//! A *runtime* argument rather than a Cargo feature, and that is the whole
//! design: features unify across a workspace, so a `conformance-decimal`
//! feature enabled by one backend is enabled for every other backend
//! compiled in the same `cargo test --workspace` — the suite would come back
//! with the decimal assertions in it exactly where they cannot pass. (It
//! did. That is how this was found.) An argument is per call site, so it
//! cannot be turned on from somewhere else, and it is a line in the
//! backend's own test naming what it does not do.
//!
//! Everything else is mandatory. A backend cannot end up with a shorter
//! suite by writing less.

use std::fmt::Debug;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone as _, Utc};
use keelson_core::{FromValue, ToValue, Value};
use keelson_exec::{ExecError, Executor, Family, Statement};
use uuid::Uuid;

/// The table this suite reads and writes.
///
/// Its own, rather than a backend's: a backend's table carries that engine's
/// extra columns, and `CREATE TABLE IF NOT EXISTS` against a differently
/// shaped table of the same name silently keeps whichever ran first.
pub const TABLE: &str = "keelson_conformance";

/// The `CREATE TABLE` for [`TABLE`] on this engine.
///
/// Only the columns all three engines have — this is the shared floor, and a
/// column one engine lacks would make the suite untestable there rather than
/// making that engine better. The types are each engine's natural home for
/// the mapped Rust type, per `docs/type-mappings.md`.
#[must_use]
pub fn ddl(family: Family) -> String {
    let body = match family {
        Family::Postgres => {
            "k bigint PRIMARY KEY,
             c_bool boolean, c_i64 int8, c_f64 float8, c_text text, c_bytes bytea,
             c_date date, c_time time, c_dt timestamp, c_tstz timestamptz,
             c_uuid uuid, c_dec numeric, c_json jsonb"
        }
        // `TIME(6)`/`DATETIME(6)` because MySQL's default second precision is
        // 0 and would truncate the fractions this suite round-trips.
        // `TIMESTAMP` is `NULL DEFAULT NULL` because MySQL otherwise makes the
        // first `TIMESTAMP` column implicitly `NOT NULL` with an auto-update.
        Family::MySql => {
            "k BIGINT PRIMARY KEY,
             c_bool TINYINT(1), c_i64 BIGINT, c_f64 DOUBLE, c_text TEXT, c_bytes BLOB,
             c_date DATE, c_time TIME(6), c_dt DATETIME(6),
             c_tstz TIMESTAMP(6) NULL DEFAULT NULL,
             c_uuid CHAR(36), c_dec DECIMAL(30, 4), c_json JSON"
        }
        // SQLite stores every mapped type in its pinned text form; the
        // declared types are affinities, and the backend's own tests are what
        // pin the exact bytes.
        Family::Sqlite => {
            "k INTEGER PRIMARY KEY,
             c_bool BOOLEAN, c_i64 INTEGER, c_f64 REAL, c_text TEXT, c_bytes BLOB,
             c_date TEXT, c_time TEXT, c_dt TEXT, c_tstz TEXT,
             c_uuid TEXT, c_dec TEXT, c_json TEXT"
        }
        // `Family` is `#[non_exhaustive]`, so a fourth engine compiles
        // against this crate before it has a table here. Loud, not a guess:
        // there is no column set that is right for an engine nobody has
        // written down.
        other => panic!("no conformance table for {other}; add one to `conformance::ddl`"),
    };
    format!("CREATE TABLE IF NOT EXISTS {TABLE} ({body})")
}

/// Process-unique row keys, so a shared server and parallel tests never
/// collide.
fn next_key() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(0);
    static BASE: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let base = *BASE.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after 1970")
            .as_nanos();
        #[allow(clippy::cast_possible_wrap)] // masked to 47 bits before the shift
        {
            (nanos as i64 & 0x7fff_ffff_ffff) << 16
        }
    });
    base + NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The backend's own placeholder syntax — these statements are raw, so
/// nothing renders them.
fn ph(family: Family, n: usize) -> String {
    match family {
        Family::Postgres => format!("${n}"),
        Family::MySql => "?".to_owned(),
        Family::Sqlite => format!("?{n}"),
        other => panic!("no placeholder syntax for {other}; add one to `conformance::ph`"),
    }
}

/// `INSERT` `v` into `col`, `SELECT` it back, return the decoded [`Value`].
async fn store(db: &dyn Executor, col: &str, v: Value) -> Result<Value, ExecError> {
    let family = db.family();
    let k = next_key();
    db.execute(Statement::new(
        format!(
            "INSERT INTO {TABLE} (k, {col}) VALUES ({}, {})",
            ph(family, 1),
            ph(family, 2)
        ),
        vec![Value::I64(k), v],
    ))
    .await?;
    let mut rows = db
        .fetch(Statement::new(
            format!("SELECT {col} FROM {TABLE} WHERE k = {}", ph(family, 1)),
            vec![Value::I64(k)],
        ))
        .await?;
    assert_eq!(rows.len(), 1, "column {col}: expected the row back");
    rows[0].take_at::<Value>(0)
}

/// The canonical round-trip: a Rust value out and back, compared with its own
/// equality rather than with the bytes an engine happened to store.
async fn rt<T>(db: &dyn Executor, col: &str, v: T)
where
    T: ToValue + FromValue + PartialEq + Debug + Clone,
{
    let out = store(db, col, v.clone().to_value())
        .await
        .unwrap_or_else(|e| panic!("column {col}: {e}"));
    let back = T::from_value(out).unwrap_or_else(|e| panic!("column {col}: {e}"));
    assert_eq!(back, v, "column {col} did not round-trip");
}

/// `None` out and back: a bound SQL `NULL` must decode as `None`, not as the
/// type's zero and not as an error.
async fn rt_none<T>(db: &dyn Executor, col: &str)
where
    T: ToValue + FromValue + PartialEq + Debug,
{
    let out = store(db, col, Value::Null)
        .await
        .unwrap_or_else(|e| panic!("column {col}: {e}"));
    let back = Option::<T>::from_value(out).unwrap_or_else(|e| panic!("column {col}: {e}"));
    assert_eq!(back, None, "column {col}: NULL did not come back as None");
}

fn fixed_uuid() -> Uuid {
    Uuid::parse_str("67e55044-10b1-426f-9247-bb680e5fe0c8").expect("a literal UUID")
}

/// A mapped type a backend may honestly not carry.
///
/// Naming one is how a backend declines part of the suite: a line in its own
/// test, not a Cargo feature that would follow the crate into every other
/// backend's build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mapped {
    /// `rust_decimal::Decimal`.
    Decimal,
}

/// Every mapped type, out and back, with the edges of each.
///
/// Call it against a live executor whose [`TABLE`] exists — see [`ddl`]. It
/// panics on the first disagreement, naming the column.
pub async fn every_mapped_type_round_trips(db: &dyn Executor) {
    every_mapped_type_round_trips_except(db, &[]).await;
}

/// The suite with the named types left out. See [`Mapped`].
pub async fn every_mapped_type_round_trips_except(db: &dyn Executor, skip: &[Mapped]) {
    // Booleans and integers.
    rt(db, "c_bool", true).await;
    rt(db, "c_bool", false).await;
    rt(db, "c_i64", i64::MAX).await;
    rt(db, "c_i64", i64::MIN).await;
    rt(db, "c_i64", 0i64).await;

    // Floats: exactly-representable values, so equality is honest.
    rt(db, "c_f64", 1.5f64).await;
    rt(db, "c_f64", -2.25f64).await;

    // Text: empty, non-BMP unicode, and a body that crosses packet
    // boundaries while staying inside MySQL's 64 KiB `TEXT` cap.
    rt(db, "c_text", String::new()).await;
    rt(db, "c_text", "crab 🦀 ∅ 日本語".to_owned()).await;
    rt(db, "c_text", "x".repeat(32 * 1024)).await;

    // Bytes: empty, embedded NUL, 0xFF.
    rt(db, "c_bytes", Vec::<u8>::new()).await;
    rt(db, "c_bytes", vec![0u8, 1, 0, 255]).await;

    // Dates: epoch, a leap day, the far end.
    rt(db, "c_date", NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()).await;
    rt(db, "c_date", NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()).await;
    rt(db, "c_date", NaiveDate::from_ymd_opt(9999, 12, 31).unwrap()).await;

    // Times: midnight, 3- and 6-digit fractions.
    rt(db, "c_time", NaiveTime::from_hms_opt(0, 0, 0).unwrap()).await;
    rt(
        db,
        "c_time",
        NaiveTime::from_hms_milli_opt(23, 59, 59, 999).unwrap(),
    )
    .await;
    rt(
        db,
        "c_time",
        NaiveTime::from_hms_micro_opt(12, 34, 56, 123_456).unwrap(),
    )
    .await;

    // Naive datetimes, with fractions.
    rt(
        db,
        "c_dt",
        NaiveDate::from_ymd_opt(2026, 7, 30)
            .unwrap()
            .and_hms_micro_opt(12, 34, 56, 789_000)
            .unwrap(),
    )
    .await;

    // Instants: a zoned bind (+09:00) must come back as the same instant in
    // UTC — the offset is consumed, never stored.
    let jst: DateTime<FixedOffset> = "2026-07-30T21:34:56.123456+09:00"
        .parse()
        .expect("a literal RFC 3339 timestamp");
    let out = store(db, "c_tstz", jst.to_value())
        .await
        .unwrap_or_else(|e| panic!("column c_tstz: {e}"));
    assert_eq!(
        DateTime::<Utc>::from_value(out).expect("a timestamptz"),
        jst.with_timezone(&Utc),
        "an offset bind did not come back as the same instant"
    );
    rt(
        db,
        "c_tstz",
        Utc.with_ymd_and_hms(2026, 7, 30, 12, 34, 56).unwrap(),
    )
    .await;

    // UUIDs: nil, max, fixed.
    rt(db, "c_uuid", Uuid::nil()).await;
    rt(db, "c_uuid", Uuid::max()).await;
    rt(db, "c_uuid", fixed_uuid()).await;

    // Decimals: numeric equality; negatives; high precision.
    if !skip.contains(&Mapped::Decimal) {
        use rust_decimal::Decimal;
        rt(db, "c_dec", Decimal::new(1999, 2)).await; // 19.99
        rt(db, "c_dec", Decimal::new(-12345, 4)).await; // -1.2345
        rt(
            db,
            "c_dec",
            "1234567890123456789012345.678"
                .parse::<Decimal>()
                .expect("a literal decimal"),
        )
        .await;
        rt_none::<Decimal>(db, "c_dec").await;
    }

    // JSON: object, array, nested unicode keys, and the "null" document —
    // which is a value, not an absence.
    rt(
        db,
        "c_json",
        serde_json::json!({"a": [1, 2], "b": {"日": "本"}}),
    )
    .await;
    rt(db, "c_json", serde_json::json!([1, "two", null])).await;
    rt(db, "c_json", serde_json::Value::Null).await;

    // NULL through every mapped type: None → None.
    rt_none::<bool>(db, "c_bool").await;
    rt_none::<i64>(db, "c_i64").await;
    rt_none::<f64>(db, "c_f64").await;
    rt_none::<String>(db, "c_text").await;
    rt_none::<Vec<u8>>(db, "c_bytes").await;
    rt_none::<NaiveDate>(db, "c_date").await;
    rt_none::<NaiveTime>(db, "c_time").await;
    rt_none::<NaiveDateTime>(db, "c_dt").await;
    rt_none::<DateTime<Utc>>(db, "c_tstz").await;
    rt_none::<Uuid>(db, "c_uuid").await;
    rt_none::<serde_json::Value>(db, "c_json").await;
}
