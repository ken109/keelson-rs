//! The write side is typed too: a `Setter` field carries the column's Rust
//! type, so `set(…)` with the wrong one does not compile. Note also what is
//! *not* an error — leaving a field out. That is `Unset`, and it means the
//! column stays out of the statement entirely.

use keelson::models::set;
use keelson_examples::models::users;

fn main() {
    let _ = users::table().insert(users::Setter {
        name: set("Ada"),
        age: set("thirty-six"),
        ..Default::default()
    });
}
