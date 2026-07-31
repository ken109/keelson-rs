//! An unbalanced brace is a mistake in the SQL, and is refused with the way to
//! write a literal one.

use keelson_sqlite::sql;

fn main() {
    let x = 1;
    let _ = sql!("SELECT * FROM t WHERE a = {x AND b = 2");
    let _ = sql!("SELECT * FROM t WHERE a = 1} AND b = 2");
    let _ = sql!("SELECT * FROM t WHERE a = {}");
}
