//! `users.age` is an INTEGER, so its column is a `Column<i64>` and comparing
//! it with a string is a type error. This is Layer 3's central promise: a
//! filter is typed by the schema, not by whatever the expression layer would
//! accept.

use keelson_examples::models::users;

fn main() {
    let _ = users::table().query(users::age().gte("twenty-one"));
}
