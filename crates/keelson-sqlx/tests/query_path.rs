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
