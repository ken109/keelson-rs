//! `within` owns the transaction's outcome: it commits on `Ok` and rolls back
//! on `Err`. The closure is handed `&Transaction`, and `commit` consumes the
//! transaction — so a closure cannot end the transaction it was given, and
//! neither a forgotten commit nor a double commit is expressible.

use keelson::exec::{BeginExt as _, ExecError};
use keelson_examples::Sandbox;

#[tokio::main]
async fn main() -> Result<(), ExecError> {
    let sandbox = Sandbox::empty().await?;
    sandbox
        .db
        .within(async |tx| {
            tx.commit().await?;
            Ok::<_, ExecError>(())
        })
        .await
}
