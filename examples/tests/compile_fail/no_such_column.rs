//! A column is a generated function, so a name the schema does not have is
//! not something you can call. Typos in column names are a compile error, not
//! a query that fails on its first run.

use keelson_examples::models::users;

fn main() {
    let _ = users::table().query(users::nickname().eq("countess"));
}
