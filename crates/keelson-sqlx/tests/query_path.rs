//! The full Layer 1 → execution path: queries built by a dialect crate, run
//! through the `Execute` verbs, mapped back through `FromRow` — the code an
//! application actually writes — plus the opt-in streaming path.

use keelson_exec::{
    ExecError, Execute as _, Executor as _, FromRow, Row, Statement, StreamExecutor as _,
};
use keelson_sqlite::{Chain as _, Query as _, arg, insert, quote, select};
use keelson_sqlx::sqlite::Pool;

#[derive(Debug, PartialEq)]
struct Person {
    id: i64,
    name: String,
    nickname: Option<String>,
}

// The documented hand-written mapping pattern — the shape codegen will emit.
impl FromRow for Person {
    fn from_row(row: &mut Row) -> Result<Self, ExecError> {
        Ok(Person {
            id: row.take("id")?,
            name: row.take("name")?,
            nickname: row.take("nickname")?,
        })
    }
}

async fn pool() -> Pool {
    let path = std::env::temp_dir().join(format!(
        "keelson-sqlx-qp-{}-{:?}.db",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&path);
    let pool = Pool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    pool.execute(Statement::new(
        "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL, nickname TEXT)",
        vec![],
    ))
    .await
    .unwrap();
    pool
}

#[tokio::test]
async fn built_queries_run_and_map() {
    let db = pool().await;

    // INSERT … RETURNING is fetched like a select — no special API.
    let q = keelson_sqlite::insert((
        insert::into("people").columns(["id", "name", "nickname"]),
        insert::values((arg(1i64), arg("ada"), arg("countess"))),
        insert::returning("id"),
    ));
    let id: i64 = q.fetch_scalar(&db).await.unwrap();
    assert_eq!(id, 1);

    let q = keelson_sqlite::insert((
        insert::into("people").columns(["id", "name", "nickname"]),
        insert::values((arg(2i64), arg("kay"), arg(None::<String>))),
    ));
    let done = q.execute(&db).await.unwrap();
    assert_eq!(done.rows_affected, 1);
    assert_eq!(done.last_insert_id, Some(2));

    // fetch_all with a mapped struct; NULL reads as None.
    let q = keelson_sqlite::select((
        select::columns((quote("id"), quote("name"), quote("nickname"))),
        select::from(quote("people")),
        select::order_by(quote("id")),
    ));
    let people: Vec<Person> = q.fetch_all(&db).await.unwrap();
    assert_eq!(
        people,
        vec![
            Person {
                id: 1,
                name: "ada".into(),
                nickname: Some("countess".into())
            },
            Person {
                id: 2,
                name: "kay".into(),
                nickname: None
            },
        ]
    );

    // fetch_one / fetch_optional semantics against a real engine.
    let one = keelson_sqlite::select((
        select::columns((quote("id"), quote("name"), quote("nickname"))),
        select::from(quote("people")),
        select::where_(quote("id").eq(arg(1i64))),
    ));
    let ada: Person = one.fetch_one(&db).await.unwrap();
    assert_eq!(ada.name, "ada");

    let missing = keelson_sqlite::select((
        select::columns((quote("id"), quote("name"), quote("nickname"))),
        select::from(quote("people")),
        select::where_(quote("id").eq(arg(99i64))),
    ));
    assert!(matches!(
        missing.fetch_optional::<Person>(&db).await,
        Ok(None)
    ));
    assert!(matches!(
        missing.fetch_one::<Person>(&db).await,
        Err(ExecError::RowNotFound)
    ));

    let all = keelson_sqlite::select((select::columns(quote("id")), select::from(quote("people"))));
    assert!(matches!(
        all.fetch_one::<(i64,)>(&db).await,
        Err(ExecError::TooManyRows)
    ));
    let ids: Vec<i64> = all.fetch_scalars(&db).await.unwrap();
    assert_eq!(ids, vec![1, 2]);

    // Decode errors name the column, end to end.
    let q = keelson_sqlite::select((
        select::columns((quote("id"), quote("nickname"), quote("name"))),
        select::from(quote("people")),
        select::where_(quote("id").eq(arg(2i64))),
    ));
    let err = q.fetch_one::<(i64, String, String)>(&db).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "column \"nickname\": cannot read NULL as String"
    );
}

#[tokio::test]
async fn streaming_hands_rows_back_incrementally() {
    let db = pool().await;
    for i in 0..100i64 {
        keelson_sqlite::insert((
            insert::into("people").columns(["id", "name"]),
            insert::values((arg(i), arg(format!("p{i}")))),
        ))
        .execute(&db)
        .await
        .unwrap();
    }

    let (sql, args) = keelson_sqlite::select((
        select::columns((quote("id"), quote("name"), quote("nickname"))),
        select::from(quote("people")),
        select::order_by(quote("id")),
    ))
    .build()
    .unwrap();

    let mut stream = db.fetch_stream(Statement::new(sql, args)).await.unwrap();
    let mut count = 0i64;
    while let Some(row) = stream.next().await {
        let mut row = row.unwrap();
        assert_eq!(row.take::<i64>("id").unwrap(), count);
        count += 1;
    }
    assert_eq!(count, 100);

    // Dropping a stream mid-way cancels the producer and releases its
    // connection — the pool stays usable.
    let mut stream = db
        .fetch_stream(Statement::new("SELECT id FROM people", vec![]))
        .await
        .unwrap();
    let _first = stream.next().await.unwrap().unwrap();
    drop(stream);
    let mut rows = db
        .fetch(Statement::new("SELECT count(id) FROM people", vec![]))
        .await
        .unwrap();
    assert_eq!(rows[0].take_at::<i64>(0).unwrap(), 100);
}

/// A statement keelson did not build gets the same verbs as one it did: the
/// point of `RawQuery` being an ordinary `Query` rather than a second API.
#[tokio::test]
async fn hand_written_statements_take_the_same_path() {
    let db = pool().await;
    keelson_sqlite::insert((
        insert::into("people").columns(["id", "name", "nickname"]),
        insert::values((arg(1i64), arg("ada"), arg("countess"))),
        insert::values((arg(2i64), arg("grace"), keelson_sqlite::raw("NULL"))),
    ))
    .execute(&db)
    .await
    .unwrap();

    // `?` is rewritten to SQLite's `?1`; the value never reaches the text.
    let q = keelson_sqlite::raw_query(
        "SELECT id, name, nickname FROM people WHERE id >= ? ORDER BY id",
    )
    .bind(1i64);
    assert_eq!(
        q.build().unwrap().0,
        "SELECT id, name, nickname FROM people WHERE id >= ?1 ORDER BY id"
    );

    // Every verb, unchanged — including mapping onto a struct.
    let all: Vec<Person> = q.fetch_all(&db).await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].nickname, None, "a NULL still decodes as None");

    let name: String = keelson_sqlite::raw_query("SELECT name FROM people WHERE id = ?")
        .bind(2i64)
        .fetch_scalar(&db)
        .await
        .unwrap();
    assert_eq!(name, "grace");

    let done = keelson_sqlite::raw_query("UPDATE people SET nickname = ? WHERE id = ?")
        .bind("amazing")
        .bind(2i64)
        .kind(keelson_sqlite::QueryType::Update)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(done.rows_affected, 1);

    // And it composes the other way: a hand-written statement spliced into a
    // built one, renumbered by the outer writer.
    let built = keelson_sqlite::select((
        select::columns((quote("id"), quote("name"), quote("nickname"))),
        select::from(quote("people")),
        select::where_(quote("id").in_(keelson_sqlite::query(
            keelson_sqlite::raw_query("SELECT id FROM people WHERE nickname = ?").bind("amazing"),
        ))),
    ));
    let (sql, args) = built.build().unwrap();
    assert_eq!(
        sql,
        concat!(
            r#"SELECT "id", "name", "nickname" FROM "people" "#,
            r#"WHERE ("id" IN (SELECT id FROM people WHERE nickname = ?1))"#
        )
    );
    assert_eq!(args.len(), 1);
    let found: Vec<Person> = built.fetch_all(&db).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "grace");
}
