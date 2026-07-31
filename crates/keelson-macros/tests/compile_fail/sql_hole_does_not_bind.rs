//! A hole binds a value, so what goes in it must be bindable. A type with no
//! `ToValue` is a compile error at the call site rather than a statement that
//! fails when it is first run.

use keelson_sqlite::sql;

struct Unbindable;

fn main() {
    let _ = sql!("SELECT * FROM t WHERE a = {Unbindable}");
}
