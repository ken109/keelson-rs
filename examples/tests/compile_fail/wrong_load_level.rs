//! A then-load path is typed by the child model: after
//! `users::then_load::posts()` the next level must load onto `Post`. Hanging a
//! loader for `User` off it is a type error, so a wrong path cannot become a
//! query that silently loads nothing.

use keelson_examples::models::users;

fn main() {
    let _ = users::then_load::posts().then(users::then_load::posts());
}
