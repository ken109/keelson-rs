//! MySQL has no `RETURNING`. keelson does not offer it and then fail at run
//! time, and does not quietly run a second statement to fetch the row back:
//! `returning` is simply not a function on this dialect.

use keelson::mysql::{self, arg, insert, quote};

fn main() {
    let _ = mysql::insert((
        insert::into(quote("users")).columns(["name"]),
        insert::values(arg("Ada")),
        insert::returning(quote("id")),
    ));
}
