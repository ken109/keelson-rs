// `#[derive(Bind)]` is for newtypes. Everything else is refused at the item
// that is not one, with the reason and the alternative.

use keelson_core::Bind;

// A struct of several fields is a row, not a value.
#[derive(Bind)]
struct Point {
    x: f64,
    y: f64,
}

// An enum needs a database representation nobody but its author can choose.
#[derive(Bind)]
enum Status {
    Draft,
    Published,
}

// Nothing to bind.
#[derive(Bind)]
struct Marker;

// Which field is live is not knowable here.
#[derive(Bind)]
union Bits {
    i: i64,
    f: f64,
}

// A borrowed newtype would derive an impl whose bound can never hold — rustc
// accepts such an impl and simply never applies it, so the derive refuses it
// here instead of leaving a dead impl behind.
#[derive(Bind)]
struct Name<'a>(&'a str);

fn main() {}
