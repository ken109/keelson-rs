// `#[derive(Bind)]` has no options: a newtype is one column with one meaning.
// Reaching for a `FromRow` option on one is the mistake worth catching.

use keelson_core::Bind;

#[derive(Bind)]
struct UserId(#[keelson(rename = "user_id")] i64);

#[derive(Bind)]
#[keelson(flatten)]
struct Email(String);

fn main() {}
