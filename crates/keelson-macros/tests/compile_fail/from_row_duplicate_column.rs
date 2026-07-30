// Reading a column consumes it — the value moves out of the row and NULL is
// left behind — so two fields reading one column would leave the second
// decoding NULL, silently. The derive can see that, so it is a compile error.

use keelson_core::FromRow;

#[derive(FromRow)]
struct Account {
    id: i64,
    #[keelson(rename = "id")]
    also_id: i64,
}

fn main() {}
