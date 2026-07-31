//! `post_authors` is a database view with no declared key, so the generator
//! emitted `View` (SELECT-only) rather than `Table`. The whole write surface
//! is bounded on `Table`: `delete` is not callable, and the `Setter` type
//! `insert`/`update` would need was never emitted. A view that cannot be
//! written through is a compile error, not a run-time refusal.

use keelson_examples::models::post_authors;

fn main() {
    let _ = post_authors::view().delete(());
}
