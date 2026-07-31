//! `&dyn Executor` is the currency for *running statements*, and it cannot
//! open a scope: an erased executor does not know whether a transaction is
//! already open, so it can be neither `within` nor `savepoint`.
//!
//! A unit of work that has to be atomic says so in its signature instead —
//! `impl Atomic`, which a pool, a `&dyn Begin` and a
//! `&Transaction` all satisfy, and which can still be passed on as
//! `&dyn Executor` to everything that is not a scope.

use keelson::exec::{ExecError, Executor};
use keelson::prelude::*;

async fn unit_of_work(db: &dyn Executor) -> Result<(), ExecError> {
    db.atomic(async |_tx| Ok::<_, ExecError>(())).await
}

fn main() {
    let _ = unit_of_work;
}
