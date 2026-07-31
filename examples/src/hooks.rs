//! The hand-written hooks the generated `users` model delegates to.
//!
//! `keelson.toml` says `[hooks] module = "crate::hooks"` and
//! `[tables.users] hooks = ["before_insert", "after_insert"]`, so the
//! generated `impl Table for Users` overrides exactly those two methods with a
//! call to the functions below. Everything else keeps the trait's default.
//!
//! Two properties worth noticing:
//!
//! - A hook receives the **caller's** executor, as `&dyn Executor`. So it runs
//!   inside the caller's transaction when there is one -- and, because it is
//!   not given a `&Transaction`, it cannot commit or roll back a transaction
//!   it did not open.
//! - `before_insert` gets the `Setter` mutably, so it can rewrite what is
//!   about to be written.

/// Hooks for the `users` table.
pub mod users {
    use keelson::exec::{ExecError, ExecFuture, Execute as _, Executor};
    use keelson::models::Set;
    use keelson::sqlite::{arg, insert, quote};

    use crate::models::users::{Setter, User};

    /// Normalise the email before the row is written.
    ///
    /// `Set` is three-state, so this must distinguish "the caller set an
    /// email" from "the caller set NULL" from "the caller said nothing" --
    /// only the first is rewritten.
    pub fn before_insert<'a>(
        _db: &'a dyn Executor,
        setter: &'a mut Setter,
    ) -> ExecFuture<'a, Result<(), ExecError>> {
        Box::pin(async move {
            if let Set::Value(email) = &mut setter.email {
                *email = email.trim().to_lowercase();
            }
            Ok(())
        })
    }

    /// Write an audit row for every user inserted, on the caller's executor.
    pub fn after_insert<'a>(
        db: &'a dyn Executor,
        rows: &'a [User],
    ) -> ExecFuture<'a, Result<(), ExecError>> {
        Box::pin(async move {
            for user in rows {
                keelson::sqlite::insert((
                    insert::into(quote("audit_logs")).columns(["entity", "entity_id", "note"]),
                    insert::values((
                        arg("users"),
                        arg(user.id),
                        arg(format!("created {}", user.name)),
                    )),
                ))
                .execute(db)
                .await?;
            }
            Ok(())
        })
    }
}
