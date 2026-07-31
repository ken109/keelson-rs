//! **Streaming a result set** instead of collecting it.
//!
//!     cargo run -p keelson-examples --example streaming
//!
//! `fetch_all` decodes the whole result set into a `Vec` before you see any of
//! it, which is right until the result set is a report or an export. The
//! backend also implements `StreamExecutor`: one row at a time, over a bounded
//! channel fed by a producer task that holds the connection.
//!
//! Two consequences worth stating plainly:
//!
//! - **The connection is held for the life of the stream.** Dropping the
//!   stream closes the channel, which the producer sees as its signal to stop
//!   and give the connection back -- so early exit is fine, and forgetting to
//!   drop it is a leaked connection.
//! - **`RowStream` is a concrete type with its own `next()`,** not
//!   `impl Stream`. That keeps the futures crate out of keelson's public API;
//!   a `Stream` impl can be added later without breaking anything.

use keelson::exec::{ExecError, Statement};
use keelson::prelude::*;
use keelson::sqlite::{self, arg, insert, quote, select};
use keelson_examples::Sandbox;

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    let sandbox = Sandbox::seeded().await?;
    let db = &sandbox.db;

    // Enough rows that streaming is not silly.
    for batch in 0..20 {
        sqlite::insert((
            insert::into(quote("comments")).columns(["post_id", "body"]),
            insert::values((arg(1), arg(format!("bulk comment {batch}")))),
        ))
        .execute(db)
        .await?;
    }

    // The streaming entry point takes a `Statement` rather than a query,
    // because it is on `Executor`'s side of the seam. `Statement::from_query`
    // is the same conversion the ergonomic verbs make internally -- it builds
    // the SQL and carries the statement kind along for tracing.
    let q = sqlite::select((
        select::columns((quote("id"), quote("body"))),
        select::from(quote("comments")),
        select::order_by(quote("id")),
    ));
    let mut stream = db.fetch_stream(Statement::from_query(&q)?).await?;

    println!("── streaming");
    let mut seen = 0;
    let mut last = String::new();
    while let Some(row) = stream.next().await {
        // Each row is a `Result`: a decode or driver failure mid-stream
        // arrives here rather than being swallowed.
        let mut row = row?;
        let body: String = row.take("body")?;
        seen += 1;
        last = body;
    }
    println!("  {seen} rows, last = {last:?}");
    assert_eq!(seen, 23);

    // ── stopping early ──────────────────────────────────────────────────
    //
    // Break out and drop the stream; the producer stops and releases its
    // connection. The pool is usable immediately afterwards, which is what
    // the next query proves.
    let mut stream = db.fetch_stream(Statement::from_query(&q)?).await?;
    let mut first_three = Vec::new();
    while let Some(row) = stream.next().await {
        first_three.push(row?.take::<i64>("id")?);
        if first_three.len() == 3 {
            break;
        }
    }
    drop(stream);

    let total: i64 = sqlite::select((
        select::columns(sqlite::f("count", quote("id"))),
        select::from(quote("comments")),
    ))
    .fetch_scalar(db)
    .await?;
    println!("\n── early exit\n  took {first_three:?} of {total} rows, pool still usable");
    assert_eq!(first_three.len(), 3);
    assert_eq!(total, 23);

    println!("\nok");
    Ok(())
}
