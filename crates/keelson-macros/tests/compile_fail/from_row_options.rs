// Every way of misusing `#[keelson(...)]` on a `FromRow` field, and the one
// option that is deliberately refused.

use keelson_core::FromRow;

#[derive(FromRow)]
struct Unknown {
    #[keelson(renmae = "id")]
    id: i64,
}

// `prefix` is not a typo and not an oversight: the message explains the
// decision and points at what to use instead.
#[derive(FromRow)]
struct Prefixed {
    #[keelson(flatten, prefix = "author_")]
    author: Author,
}

#[derive(FromRow)]
struct Author {
    id: i64,
}

// One names a single column, the other reads many.
#[derive(FromRow)]
struct Both {
    #[keelson(rename = "author_id", flatten)]
    author: Author,
}

#[derive(FromRow)]
struct EmptyName {
    #[keelson(rename = "")]
    id: i64,
}

#[derive(FromRow)]
struct NotAString {
    #[keelson(rename = 3)]
    id: i64,
}

#[derive(FromRow)]
struct NoValue {
    #[keelson(rename)]
    id: i64,
}

#[derive(FromRow)]
struct FlattenWithValue {
    #[keelson(flatten = "author")]
    author: Author,
}

#[derive(FromRow)]
struct Twice {
    #[keelson(rename = "a")]
    #[keelson(rename = "b")]
    id: i64,
}

#[derive(FromRow)]
struct NotAList {
    #[keelson = "id"]
    id: i64,
}

// Options describe one field, so the struct itself understands none.
#[derive(FromRow)]
#[keelson(rename = "accounts")]
struct OnTheStruct {
    id: i64,
}

fn main() {}
