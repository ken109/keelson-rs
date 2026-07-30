//! The observability policy, pinned: the SQL text is recorded, the argument
//! *values* never are — at any level, on any field. This test IS the policy.
//!
//! Its own test binary on purpose: tracing caches per-callsite interest
//! globally, so a test that runs the library's spans *before* any subscriber
//! exists could freeze them disabled for the process. One test, one process,
//! no interference.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use keelson_core::testing::Numbered;
use keelson_core::{Dialect, Expression, Query, QueryType, SqlWriter, Value};
use keelson_exec::{
    Column, ExecError, ExecFuture, ExecResult, Execute, Executor, Family, Row, Statement,
};
use tracing::field::{Field, Visit};
use tracing::span;

#[derive(Debug, Default)]
struct Scripted;

impl Executor for Scripted {
    fn family(&self) -> Family {
        Family::Sqlite
    }

    fn fetch(&self, _stmt: Statement) -> ExecFuture<'_, Result<Vec<Row>, ExecError>> {
        let header: Arc<[Column]> = vec![Column::new("id")].into();
        let rows = vec![Row::new(header, vec![Value::I64(7)])];
        Box::pin(async move { Ok(rows) })
    }

    fn execute(&self, _stmt: Statement) -> ExecFuture<'_, Result<ExecResult, ExecError>> {
        Box::pin(async move { Ok(ExecResult::new(3, None)) })
    }
}

#[derive(Clone, Default)]
struct Capture {
    out: Arc<Mutex<String>>,
}

struct Visitor(Arc<Mutex<String>>);

impl Visit for Visitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut out = self.0.lock().unwrap();
        let _ = writeln!(out, "{}={:?}", field.name(), value);
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &span::Attributes<'_>) -> span::Id {
        span.record(&mut Visitor(self.out.clone()));
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, values: &span::Record<'_>) {
        values.record(&mut Visitor(self.out.clone()));
    }

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut Visitor(self.out.clone()));
    }

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}
}

#[derive(Debug)]
struct ByName(&'static str);

impl Expression for ByName {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        w.push_str("SELECT * FROM users WHERE name = ");
        w.push_arg(self.0);
    }
}

impl Query for ByName {
    fn query_type(&self) -> QueryType {
        QueryType::Select
    }

    fn dialect(&self) -> &dyn Dialect {
        &Numbered
    }
}

#[test]
fn sql_is_recorded_and_args_never_are() {
    const SECRET: &str = "the-users-taxpayer-id-000-00-0000";
    let capture = Capture::default();
    let out = capture.out.clone();

    tracing::subscriber::with_default(capture, || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let db = Scripted;
            let _rows: Vec<Row> = ByName(SECRET).fetch_all(&db).await.unwrap();
            let _ = ByName(SECRET).execute(&db).await.unwrap();
        });
    });

    let captured = out.lock().unwrap();
    assert!(
        captured.contains("SELECT * FROM users WHERE name = $1"),
        "the SQL text (placeholders, no data) must be recorded:\n{captured}"
    );
    assert!(
        captured.contains("keelson.args.count=1"),
        "the argument count must be recorded:\n{captured}"
    );
    assert!(
        captured.contains("keelson.rows=1"),
        "the row count must be recorded on close:\n{captured}"
    );
    assert!(
        captured.contains("keelson.rows_affected=3"),
        "the affected-row count must be recorded on close:\n{captured}"
    );
    assert!(
        !captured.contains(SECRET),
        "an argument value leaked into telemetry:\n{captured}"
    );
}
