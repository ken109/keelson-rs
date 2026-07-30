// `#[derive(FromRow)]` matches fields to columns by name, so it needs names.

use keelson_core::FromRow;

// Tuples already implement FromRow positionally; the error says so with these
// very types substituted in.
#[derive(FromRow)]
struct Pair(i64, String);

// Nothing to map onto.
#[derive(FromRow)]
struct Nothing;

// Which variant a row is depends on a discriminator only the author can name.
#[derive(FromRow)]
enum Event {
    Created { at: String },
    Deleted,
}

// Same for a borrowed mapper: a row is decoded into owned values.
#[derive(FromRow)]
struct Borrowed<'a> {
    name: &'a str,
}

fn main() {}
