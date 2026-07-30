//! The `Execute` verbs against a scripted executor — the row-count semantics,
//! the `&dyn Executor` currency, and the tracing policy, all without a
//! database.

use std::sync::{Arc, Mutex};

use keelson_core::testing::Numbered;
use keelson_core::{Dialect, Expression, Query, QueryType, SqlWriter, Value};
use keelson_exec::{
    Column, ExecError, ExecFuture, ExecResult, Execute, Executor, Family, Row, Statement,
};

/// A query the shape a dialect crate produces.
#[derive(Debug)]
struct Select {
    min_age: i32,
}

impl Expression for Select {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str("SELECT * FROM ");
        w.push_quoted(&["users"]);
        w.push_str(" WHERE ");
        w.push_quoted(&["age"]);
        w.push_str(" >= ");
        w.push_arg(self.min_age);
    }
}

impl Query for Select {
    fn query_type(&self) -> QueryType {
        QueryType::Select
    }

    fn dialect(&self) -> &dyn Dialect {
        &Numbered
    }
}

/// An executor that hands back a scripted result set and records what it was
/// asked to run.
#[derive(Debug, Default)]
struct Scripted {
    rows: Vec<Vec<Value>>,
    seen: Arc<Mutex<Vec<Statement>>>,
}

impl Scripted {
    fn returning(rows: Vec<Vec<Value>>) -> Self {
        Scripted {
            rows,
            seen: Arc::default(),
        }
    }

    fn make_rows(&self) -> Vec<Row> {
        let header: Arc<[Column]> = vec![Column::new("id"), Column::new("name")].into();
        self.rows
            .iter()
            .map(|values| Row::new(header.clone(), values.clone()))
            .collect()
    }
}

impl Executor for Scripted {
    fn family(&self) -> Family {
        Family::Sqlite
    }

    fn fetch(&self, stmt: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>> {
        self.seen.lock().unwrap().push(stmt);
        let rows = self.make_rows();
        Box::pin(async move { Ok(rows) })
    }

    fn execute(&self, stmt: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>> {
        self.seen.lock().unwrap().push(stmt);
        Box::pin(async move { Ok(ExecResult::new(3, Some(42))) })
    }
}

fn one_row() -> Scripted {
    Scripted::returning(vec![vec![Value::I64(7), Value::Text("ada".into())]])
}

fn two_rows() -> Scripted {
    Scripted::returning(vec![
        vec![Value::I64(7), Value::Text("ada".into())],
        vec![Value::I64(8), Value::Text("kay".into())],
    ])
}

#[tokio::test]
async fn the_verbs_build_and_dispatch() {
    let db = one_row();
    let q = Select { min_age: 21 };

    let rows: Vec<(i64, String)> = q.fetch_all(&db).await.unwrap();
    assert_eq!(rows, vec![(7, "ada".into())]);

    let (id, _): (i64, String) = q.fetch_one(&db).await.unwrap();
    assert_eq!(id, 7);

    let id: i64 = q.fetch_scalar(&db).await.unwrap();
    assert_eq!(id, 7);

    let ids: Vec<i64> = q.fetch_scalars(&db).await.unwrap();
    assert_eq!(ids, vec![7]);

    let done = q.execute(&db).await.unwrap();
    assert_eq!(done.rows_affected, 3);
    assert_eq!(done.last_insert_id, Some(42));

    // What reached the executor is the built statement, unchanged.
    let seen = db.seen.lock().unwrap();
    assert_eq!(seen[0].sql, r#"SELECT * FROM "users" WHERE "age" >= $1"#);
    assert_eq!(seen[0].args, vec![Value::I32(21)]);
    assert_eq!(seen[0].query_type, QueryType::Select);
}

#[tokio::test]
async fn one_means_one() {
    let q = Select { min_age: 21 };

    let none = Scripted::returning(vec![]);
    assert!(matches!(
        q.fetch_one::<Row>(&none).await,
        Err(ExecError::RowNotFound)
    ));
    assert!(q.fetch_optional::<Row>(&none).await.unwrap().is_none());

    let two = two_rows();
    assert!(matches!(
        q.fetch_one::<Row>(&two).await,
        Err(ExecError::TooManyRows)
    ));
    // sqlx would silently take the first; keelson refuses even on optional.
    assert!(matches!(
        q.fetch_optional::<Row>(&two).await,
        Err(ExecError::TooManyRows)
    ));
}

#[tokio::test]
async fn a_dyn_executor_is_the_currency() {
    // The design's center of gravity: one function, pool or transaction alike.
    async fn count(db: &dyn Executor) -> Result<i64, ExecError> {
        Select { min_age: 0 }.fetch_scalar(db).await
    }
    let db = one_row();
    assert_eq!(count(&db).await.unwrap(), 7);
}
