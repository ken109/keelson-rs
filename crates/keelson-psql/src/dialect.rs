use keelson_core::Dialect;

/// PostgreSQL: `$1` placeholders, `"` quoting, no named arguments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Psql;

impl Dialect for Psql {
    fn write_arg(&self, w: &mut String, position: usize) {
        w.push('$');
        w.push_str(&position.to_string());
    }

    fn write_quoted(&self, w: &mut String, s: &str) {
        w.push('"');
        w.push_str(s);
        w.push('"');
    }
}

/// The dialect every `keelson-psql` query builds with.
///
/// A `static` rather than a `const` so that `&PSQL` is a `&'static dyn Dialect`
/// without relying on promotion.
pub static PSQL: Psql = Psql;
