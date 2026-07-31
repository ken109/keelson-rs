//! `sql!` is not `format!`: the values go in the holes, not after the string.
//! Accepting trailing arguments would make the positional-binding mistake the
//! macro exists to remove expressible again.

use keelson_sqlite::sql;

fn main() {
    let x = 1;
    let _ = sql!("SELECT * FROM t WHERE a = {}", x);
}
