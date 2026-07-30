//! Round-trip tests for every mapped type in `docs/type-mappings.md`:
//! bind → `INSERT` → `SELECT` back → compare with the type's own semantic
//! equality. This is the executable form of the mappings table — the tests of
//! each backend's `bind_value` and `decode_value` pair.
//!
//! Real SQLite always (in-process, so `cargo test` exercises the whole
//! harness); real PostgreSQL 17 and MySQL 8.4 behind the `live-docker`
//! feature, reusing keelson-sqlcheck's containers and `KEELSON_LIVE_*_URL`
//! overrides:
//!
//! ```sh
//! cargo test -p keelson-sqlx --features live-docker
//! ```

use std::fmt::Debug;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone as _, Utc};
use keelson_core::{FromValue, ToValue, Value};
use keelson_exec::{ExecError, Executor, Family, Statement};
use rust_decimal::Decimal;
use uuid::Uuid;

/// Process-unique row keys, so shared servers and parallel tests never
/// collide.
fn next_key() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(0);
    static BASE: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let base = *BASE.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        (nanos as i64 & 0x7fff_ffff_ffff) << 16
    });
    base + NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The backend's own placeholder syntax — raw statements are passed verbatim.
fn ph(family: Family, n: usize) -> String {
    match family {
        Family::Postgres => format!("${n}"),
        Family::MySql => "?".to_owned(),
        Family::Sqlite => format!("?{n}"),
        _ => unreachable!(),
    }
}

/// INSERT `v` into `col`, SELECT it back, return the decoded `Value`.
async fn store(db: &dyn Executor, col: &str, v: Value) -> Result<Value, ExecError> {
    let family = db.family();
    let k = next_key();
    let sql = format!(
        "INSERT INTO keelson_roundtrip (k, {col}) VALUES ({}, {})",
        ph(family, 1),
        ph(family, 2)
    );
    db.execute(Statement::new(sql, vec![Value::I64(k), v]))
        .await?;
    let sql = format!(
        "SELECT {col} FROM keelson_roundtrip WHERE k = {}",
        ph(family, 1)
    );
    let mut rows = db.fetch(Statement::new(sql, vec![Value::I64(k)])).await?;
    assert_eq!(rows.len(), 1, "column {col}: expected the row back");
    rows[0].take_at::<Value>(0)
}

/// The canonical round-trip: a Rust value out and back, compared with its own
/// equality.
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

/// `None` for every mapped type: NULL out, NULL back.
async fn rt_none<T>(db: &dyn Executor, col: &str)
where
    T: ToValue + FromValue + PartialEq + Debug + Clone,
{
    rt::<Option<T>>(db, col, None).await;
}

fn fixed_uuid() -> Uuid {
    Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
}

/// The shared per-family suite: every mapped type, canonical values plus the
/// edges the mappings doc pins. Columns that exist on every family.
async fn common_suite(db: &dyn Executor) {
    // Booleans and integers.
    rt(db, "c_bool", true).await;
    rt(db, "c_bool", false).await;
    rt(db, "c_i64", i64::MAX).await;
    rt(db, "c_i64", i64::MIN).await;
    rt(db, "c_i64", 0i64).await;

    // Floats: exactly-representable values, so equality is honest.
    rt(db, "c_f64", 1.5f64).await;
    rt(db, "c_f64", -2.25f64).await;

    // Text: empty, non-BMP unicode, and a 64 KiB body.
    rt(db, "c_text", String::new()).await;
    rt(db, "c_text", "crab 🦀 ∅ 日本語".to_owned()).await;
    // 32 KiB: large enough to cross packet boundaries, inside MySQL's 64
    // KiB TEXT cap.
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
    let dt = NaiveDate::from_ymd_opt(2026, 7, 30)
        .unwrap()
        .and_hms_micro_opt(12, 34, 56, 789_000)
        .unwrap();
    rt(db, "c_dt", dt).await;

    // Instants: a zoned bind (+09:00) must come back as the same instant in
    // UTC — the offset is consumed, never stored.
    let jst: DateTime<FixedOffset> = "2026-07-30T21:34:56.123456+09:00".parse().unwrap();
    let utc = jst.with_timezone(&Utc);
    let out = store(db, "c_tstz", jst.to_value()).await.unwrap();
    assert_eq!(DateTime::<Utc>::from_value(out).unwrap(), utc);
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
    rt(db, "c_dec", Decimal::new(1999, 2)).await; // 19.99
    rt(db, "c_dec", Decimal::new(-12345, 4)).await; // -1.2345
    rt(
        db,
        "c_dec",
        "1234567890123456789012345.678".parse::<Decimal>().unwrap(),
    )
    .await;

    // JSON: object, array, nested unicode keys, and the "null" document.
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
    rt_none::<Decimal>(db, "c_dec").await;
    rt_none::<serde_json::Value>(db, "c_json").await;
}

// ───────────────────────────── SQLite (always) ─────────────────────────────

mod sqlite_engine {
    use super::*;
    use keelson_sqlx::sqlite::Pool;

    const DDL: &str = "CREATE TABLE IF NOT EXISTS keelson_roundtrip (
        k INTEGER PRIMARY KEY,
        c_bool BOOLEAN, c_i64 INTEGER, c_f64 REAL, c_text TEXT, c_bytes BLOB,
        c_date TEXT, c_time TEXT, c_dt TEXT, c_tstz TEXT,
        c_uuid TEXT, c_dec TEXT, c_json TEXT)";

    /// A fresh pool per test: sqlx pools are runtime-bound, and every
    /// `#[tokio::test]` is its own runtime.
    pub(crate) async fn pool() -> Pool {
        let path = std::env::temp_dir().join(format!(
            "keelson-sqlx-roundtrip-{}-{}.db",
            std::process::id(),
            next_key()
        ));
        let pool = Pool::connect(&format!("sqlite://{}", path.display()))
            .await
            .expect("opening the SQLite database");
        pool.execute(Statement::new(DDL, vec![])).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn every_mapped_type_round_trips() {
        common_suite(&pool().await).await;
    }

    /// SQLite stores every mapped type as its pinned text form; the exact
    /// bytes are part of the contract (they are what `FromValue` accepts and
    /// what sorts correctly).
    #[tokio::test]
    async fn the_stored_text_is_the_pinned_form() {
        let db = &pool().await;
        for (col, v, expected) in [
            (
                "c_date",
                NaiveDate::from_ymd_opt(2026, 7, 30).unwrap().to_value(),
                "2026-07-30",
            ),
            (
                "c_time",
                NaiveTime::from_hms_opt(12, 34, 56).unwrap().to_value(),
                "12:34:56",
            ),
            (
                "c_dt",
                NaiveDate::from_ymd_opt(2026, 7, 30)
                    .unwrap()
                    .and_hms_opt(12, 34, 56)
                    .unwrap()
                    .to_value(),
                "2026-07-30T12:34:56",
            ),
            (
                "c_tstz",
                Utc.with_ymd_and_hms(2026, 7, 30, 12, 34, 56)
                    .unwrap()
                    .to_value(),
                "2026-07-30T12:34:56Z",
            ),
            (
                "c_uuid",
                fixed_uuid().to_value(),
                "550e8400-e29b-41d4-a716-446655440000",
            ),
            // Scale preserved: 1.10 stays 1.10, not 1.1.
            ("c_dec", Decimal::new(110, 2).to_value(), "1.10"),
        ] {
            let out = store(db, col, v).await.unwrap();
            assert_eq!(out, Value::Text(expected.into()), "column {col}");
        }
    }

    /// The space-separated datetime SQLite and MySQL conventionally store
    /// must read back — `FromValue`'s documented text acceptance.
    #[tokio::test]
    async fn space_separated_datetimes_read_back() {
        let db = &pool().await;
        let out = store(db, "c_dt", Value::Text("2026-07-30 12:34:56".into()))
            .await
            .unwrap();
        let dt = NaiveDateTime::from_value(out).unwrap();
        assert_eq!(
            dt,
            NaiveDate::from_ymd_opt(2026, 7, 30)
                .unwrap()
                .and_hms_opt(12, 34, 56)
                .unwrap()
        );
    }

    /// u64 beyond i64 has no SQLite representation: refused at bind, loudly.
    #[tokio::test]
    async fn u64_beyond_i64_is_refused() {
        let db = &pool().await;
        let err = store(db, "c_i64", Value::U64(u64::MAX)).await.unwrap_err();
        assert!(matches!(err, ExecError::UnsupportedValue { .. }), "{err}");
    }

    /// An unknown CustomValue is refused at bind time by name — never
    /// silently stringified.
    #[tokio::test]
    async fn unknown_custom_values_are_refused() {
        #[derive(Debug)]
        struct Point;
        impl keelson_core::CustomValue for Point {
            fn type_name(&self) -> &'static str {
                "point"
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }
        let db = &pool().await;
        let err = store(db, "c_text", Value::custom(Point)).await.unwrap_err();
        assert_eq!(err.to_string(), "cannot bind a point value on sqlite");
    }
}

// ─────────────────── PostgreSQL + MySQL (live-docker) ───────────────────

#[cfg(feature = "live-docker")]
mod live_engines {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Run `ddl` exactly once per process, serialised across parallel tests
    /// (each test is its own runtime, so pools cannot be shared — but the
    /// server is).
    async fn ensure_ddl(db: &dyn Executor, done: &StdMutex<bool>, ddl: &str) {
        let mut done = done.lock().unwrap();
        if !*done {
            db.execute(Statement::new(ddl, vec![])).await.unwrap();
            *done = true;
        }
    }

    /// A fresh pool per test (sqlx pools are runtime-bound); the container
    /// and its URL are shared through keelson-sqlcheck.
    pub(crate) async fn psql_pool() -> keelson_sqlx::psql::Pool {
        static DDL_DONE: StdMutex<bool> = StdMutex::new(false);
        // Container startup is blocking (sqlcheck's SyncRunner).
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::psql_url().to_owned())
            .await
            .unwrap();
        let pool = keelson_sqlx::psql::Pool::connect(&url)
            .await
            .expect("connecting to the live PostgreSQL");
        ensure_ddl(
            &pool,
            &DDL_DONE,
            "CREATE TABLE IF NOT EXISTS keelson_roundtrip (
                k bigint PRIMARY KEY,
                c_bool boolean, c_i16 int2, c_i32 int4, c_i64 int8,
                c_f32 float4, c_f64 float8, c_text text, c_bytes bytea,
                c_date date, c_time time, c_dt timestamp, c_tstz timestamptz,
                c_uuid uuid, c_dec numeric, c_json jsonb,
                c_arr_i64 int8[], c_arr_text text[])",
        )
        .await;
        pool
    }

    pub(crate) async fn mysql_pool() -> keelson_sqlx::mysql::Pool {
        static DDL_DONE: StdMutex<bool> = StdMutex::new(false);
        let url = tokio::task::spawn_blocking(|| keelson_sqlcheck::live::mysql_url().to_owned())
            .await
            .unwrap();
        let pool = keelson_sqlx::mysql::Pool::connect(&url)
            .await
            .expect("connecting to the live MySQL");
        ensure_ddl(
            &pool,
            &DDL_DONE,
            "CREATE TABLE IF NOT EXISTS keelson_roundtrip (
                k BIGINT PRIMARY KEY,
                c_bool TINYINT(1), c_i8 TINYINT, c_i16 SMALLINT, c_i32 INT,
                c_i64 BIGINT, c_u8 TINYINT UNSIGNED, c_u16 SMALLINT UNSIGNED,
                c_u32 INT UNSIGNED, c_u64 BIGINT UNSIGNED,
                c_f32 FLOAT, c_f64 DOUBLE, c_text TEXT, c_bytes BLOB,
                c_date DATE, c_time TIME(6), c_dt DATETIME(6),
                c_tstz TIMESTAMP(6) NULL DEFAULT NULL,
                c_uuid CHAR(36), c_uuid_bin BINARY(16),
                c_dec DECIMAL(30, 4), c_json JSON)",
        )
        .await;
        pool
    }

    #[tokio::test]
    async fn psql_every_mapped_type_round_trips() {
        let db = &psql_pool().await;
        common_suite(db).await;

        // The narrower integer and float widths PostgreSQL types natively.
        rt(db, "c_i16", i16::MIN).await;
        rt(db, "c_i16", i16::MAX).await;
        rt(db, "c_i32", i32::MIN).await;
        rt(db, "c_i32", i32::MAX).await;
        rt(db, "c_f32", 1.5f32).await;
    }

    #[tokio::test]
    async fn psql_arrays_round_trip() {
        let db = &psql_pool().await;
        // Typed arrays, with a NULL element riding along.
        let out = store(
            db,
            "c_arr_i64",
            Value::Array(vec![Value::I64(1), Value::Null, Value::I64(3)]),
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            Value::Array(vec![Value::I64(1), Value::Null, Value::I64(3)])
        );

        let out = store(
            db,
            "c_arr_text",
            Value::Array(vec![Value::Text("a".into()), Value::Text("".into())]),
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            Value::Array(vec![Value::Text("a".into()), Value::Text("".into())])
        );

        // Empty arrays bind as text[] — fine where the column is text[].
        let out = store(db, "c_arr_text", Value::Array(vec![])).await.unwrap();
        assert_eq!(out, Value::Array(vec![]));
    }

    /// Scale preservation, asserted on the *text* the server renders:
    /// unconstrained `numeric` keeps the bound scale, `1.10` stays `1.10`.
    #[tokio::test]
    async fn psql_decimal_scale_survives() {
        let db = &psql_pool().await;
        let k = next_key();
        db.execute(Statement::new(
            "INSERT INTO keelson_roundtrip (k, c_dec) VALUES ($1, $2)",
            vec![Value::I64(k), Decimal::new(110, 2).to_value()],
        ))
        .await
        .unwrap();
        let mut rows = db
            .fetch(Statement::new(
                "SELECT CAST(c_dec AS text) FROM keelson_roundtrip WHERE k = $1",
                vec![Value::I64(k)],
            ))
            .await
            .unwrap();
        assert_eq!(rows[0].take_at::<String>(0).unwrap(), "1.10");
    }

    /// The pinned text forms agree with the native binds: inserting the text
    /// form (through an explicit cast, PostgreSQL being strictly typed) reads
    /// back as the same native value.
    #[tokio::test]
    async fn psql_text_forms_agree_with_native_binds() {
        let db = &psql_pool().await;
        for (col, cast, text, native) in [
            (
                "c_date",
                "date",
                "2026-07-30",
                NaiveDate::from_ymd_opt(2026, 7, 30).unwrap().to_value(),
            ),
            (
                "c_uuid",
                "uuid",
                "550e8400-e29b-41d4-a716-446655440000",
                fixed_uuid().to_value(),
            ),
            (
                "c_tstz",
                "timestamptz",
                "2026-07-30T12:34:56Z",
                Utc.with_ymd_and_hms(2026, 7, 30, 12, 34, 56)
                    .unwrap()
                    .to_value(),
            ),
        ] {
            let k = next_key();
            db.execute(Statement::new(
                format!("INSERT INTO keelson_roundtrip (k, {col}) VALUES ($1, CAST($2 AS {cast}))"),
                vec![Value::I64(k), Value::Text(text.into())],
            ))
            .await
            .unwrap();
            let mut rows = db
                .fetch(Statement::new(
                    format!("SELECT {col} FROM keelson_roundtrip WHERE k = $1"),
                    vec![Value::I64(k)],
                ))
                .await
                .unwrap();
            assert_eq!(rows[0].take_at::<Value>(0).unwrap(), native, "column {col}");
        }
    }

    #[tokio::test]
    async fn mysql_every_mapped_type_round_trips() {
        let db = &mysql_pool().await;
        common_suite(db).await;

        // The full signed and unsigned ladder MySQL types natively.
        rt(db, "c_i8", i8::MIN).await;
        rt(db, "c_i16", i16::MAX).await;
        rt(db, "c_i32", i32::MIN).await;
        rt(db, "c_u8", u8::MAX).await;
        rt(db, "c_u16", u16::MAX).await;
        rt(db, "c_u32", u32::MAX).await;
        rt(db, "c_u64", u64::MAX).await;
        rt(db, "c_f32", 1.5f32).await;
    }

    /// The session-zone pin, *tested* rather than trusted: every pooled
    /// connection answers `+00:00`, and a zoned bind stores the instant.
    #[tokio::test]
    async fn mysql_session_zone_is_pinned() {
        let db = &mysql_pool().await;
        for _ in 0..3 {
            let mut rows = db
                .fetch(Statement::new("SELECT @@session.time_zone", vec![]))
                .await
                .unwrap();
            assert_eq!(rows[0].take_at::<String>(0).unwrap(), "+00:00");
        }
    }

    /// MySQL accepts the pinned text forms directly (string coercion), so the
    /// text fallback and the native bind agree.
    #[tokio::test]
    async fn mysql_text_forms_agree_with_native_binds() {
        let db = &mysql_pool().await;
        let out = store(db, "c_date", Value::Text("2026-07-30".into()))
            .await
            .unwrap();
        assert_eq!(
            NaiveDate::from_value(out).unwrap(),
            NaiveDate::from_ymd_opt(2026, 7, 30).unwrap()
        );
        // The space-separated form MySQL conventionally stores.
        let out = store(db, "c_dt", Value::Text("2026-07-30 12:34:56".into()))
            .await
            .unwrap();
        assert_eq!(
            NaiveDateTime::from_value(out).unwrap(),
            NaiveDate::from_ymd_opt(2026, 7, 30)
                .unwrap()
                .and_hms_opt(12, 34, 56)
                .unwrap()
        );
    }

    /// `BINARY(16)` is not the standard uuid mapping, but 16 raw bytes must
    /// read back as a Uuid — the documented acceptance.
    #[tokio::test]
    async fn mysql_binary_16_uuid_reads_back() {
        let db = &mysql_pool().await;
        let u = fixed_uuid();
        let out = store(db, "c_uuid_bin", Value::Bytes(u.as_bytes().to_vec()))
            .await
            .unwrap();
        assert_eq!(Uuid::from_value(out).unwrap(), u);
    }

    /// Cross-backend agreement, tested literally: one canonical value per
    /// mapped type, round-tripped on all three engines, equal everywhere.
    #[tokio::test]
    async fn all_three_backends_agree() {
        let sqlite_pool = super::sqlite_engine::pool().await;
        let psql_pool_ = psql_pool().await;
        let mysql_pool_ = mysql_pool().await;
        let sqlite = &sqlite_pool as &dyn Executor;
        let psql = &psql_pool_ as &dyn Executor;
        let mysql = &mysql_pool_ as &dyn Executor;

        macro_rules! agree {
            ($col:expr, $t:ty, $v:expr) => {{
                let v: $t = $v;
                let mut got: Vec<$t> = Vec::new();
                for db in [sqlite, psql, mysql] {
                    let out = store(db, $col, v.clone().to_value()).await.unwrap();
                    got.push(<$t>::from_value(out).unwrap());
                }
                assert!(
                    got.iter().all(|g| *g == v),
                    "backends disagree on {}: {:?}",
                    $col,
                    got
                );
            }};
        }

        agree!("c_bool", bool, true);
        agree!("c_i64", i64, -42);
        agree!("c_f64", f64, 2.5);
        agree!("c_text", String, "άπαξ 🦀".to_owned());
        agree!("c_bytes", Vec<u8>, vec![0u8, 255]);
        agree!(
            "c_date",
            NaiveDate,
            NaiveDate::from_ymd_opt(2026, 7, 30).unwrap()
        );
        agree!(
            "c_time",
            NaiveTime,
            NaiveTime::from_hms_milli_opt(12, 34, 56, 789).unwrap()
        );
        agree!(
            "c_dt",
            NaiveDateTime,
            NaiveDate::from_ymd_opt(2026, 7, 30)
                .unwrap()
                .and_hms_opt(12, 34, 56)
                .unwrap()
        );
        agree!(
            "c_tstz",
            DateTime<Utc>,
            Utc.with_ymd_and_hms(2026, 7, 30, 12, 34, 56).unwrap()
        );
        agree!("c_uuid", Uuid, fixed_uuid());
        agree!("c_dec", Decimal, Decimal::new(1999, 2));
        agree!(
            "c_json",
            serde_json::Value,
            serde_json::json!({"a": [1, 2, 3], "b": "x"})
        );
    }
}
