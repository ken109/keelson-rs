//! The trade `examples/repositories.rs` makes explicit: a repository trait
//! can take a *scope* or it can be a trait object, not both.
//!
//! `&(impl Atomic + ?Sized)` is a generic parameter, so a trait with such a
//! method has no vtable and `Arc<dyn UserRepository>` stops compiling. Take
//! `&dyn Executor` in the port and let the usecase own the transaction, or
//! keep the repository generic and give up `dyn`.

use std::sync::Arc;

use keelson::exec::ExecError;
use keelson::prelude::*;

trait UserRepository: Send + Sync {
    fn deactivate(
        &self,
        db: &(impl Atomic + ?Sized),
        id: i64,
    ) -> impl Future<Output = Result<(), ExecError>>;
}

fn wire(repo: Arc<dyn UserRepository>) {
    let _ = repo;
}

fn main() {
    let _ = wire;
}
