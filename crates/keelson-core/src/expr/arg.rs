use crate::error::Result;
use crate::value::{ToValue, Value};
use crate::writer::{Expression, SqlWriter};

/// A comma-separated run of bound arguments, optionally parenthesised.
///
/// This is where placeholder numbering actually happens: rendering an `Args`
/// calls [`SqlWriter::push_arg`] once per value, which is the only thing that
/// advances the counter.
#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    vals: Vec<Value>,
    grouped: bool,
}

impl Args {
    /// `$1, $2, $3`.
    pub fn new<V: ToValue>(vals: impl IntoIterator<Item = V>) -> Self {
        Args {
            vals: vals.into_iter().map(ToValue::to_value).collect(),
            grouped: false,
        }
    }

    /// `($1, $2, $3)` — for row constructors and `VALUES` tuples.
    pub fn grouped<V: ToValue>(vals: impl IntoIterator<Item = V>) -> Self {
        Args {
            vals: vals.into_iter().map(ToValue::to_value).collect(),
            grouped: true,
        }
    }

    /// The bound values, in placeholder order.
    pub fn values(&self) -> &[Value] {
        &self.vals
    }

    /// Whether the run is wrapped in parentheses.
    pub fn is_grouped(&self) -> bool {
        self.grouped
    }
}

/// One bound argument.
pub fn arg(v: impl ToValue) -> Args {
    Args::new([v])
}

/// A comma-separated run of bound arguments — bob's `Arg`.
pub fn args<V: ToValue>(vals: impl IntoIterator<Item = V>) -> Args {
    Args::new(vals)
}

/// [`args`] wrapped in parentheses — bob's `ArgGroup`.
pub fn arg_group<V: ToValue>(vals: impl IntoIterator<Item = V>) -> Args {
    Args::grouped(vals)
}

/// `n` placeholders bound to `NULL`, for a statement that will be prepared and
/// executed with real values later — bob's `Placeholder`.
pub fn placeholders(n: usize) -> Args {
    Args {
        vals: vec![Value::Null; n],
        grouped: false,
    }
}

impl Expression for Args {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.grouped {
            w.push_str("(");
        }

        // An empty run still has to render as something, and `NULL` is the one
        // spelling that is valid everywhere a value list is.
        if self.vals.is_empty() {
            w.push_str("NULL");
        }

        for (i, v) in self.vals.iter().enumerate() {
            if i > 0 {
                w.push_str(", ");
            }
            w.push_arg(v.clone());
        }

        if self.grouped {
            w.push_str(")");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::{Numbered, Positional};
    use crate::writer::{build, build_from};

    #[test]
    fn a_run_numbers_from_the_writer_position() {
        let (sql, vals) = build(&Numbered, &args([10, 20, 30])).unwrap();
        assert_eq!(sql, "$1, $2, $3");
        assert_eq!(vals, vec![Value::I32(10), Value::I32(20), Value::I32(30)]);

        let (sql, _) = build_from(&Numbered, 4, &args([10, 20])).unwrap();
        assert_eq!(sql, "$4, $5");
    }

    #[test]
    fn a_group_brings_its_own_parentheses() {
        let (sql, _) = build(&Numbered, &arg_group([1, 2])).unwrap();
        assert_eq!(sql, "($1, $2)");
    }

    #[test]
    fn an_empty_run_is_null() {
        let (sql, vals) = build(&Numbered, &args(Vec::<i32>::new())).unwrap();
        assert_eq!(sql, "NULL");
        assert!(vals.is_empty());

        let (sql, _) = build(&Numbered, &arg_group(Vec::<i32>::new())).unwrap();
        assert_eq!(sql, "(NULL)");
    }

    #[test]
    fn placeholders_bind_nulls() {
        let (sql, vals) = build(&Numbered, &placeholders(3)).unwrap();
        assert_eq!(sql, "$1, $2, $3");
        assert_eq!(vals, vec![Value::Null, Value::Null, Value::Null]);
    }

    #[test]
    fn a_positional_dialect_drops_the_index_but_keeps_the_order() {
        let (sql, vals) = build(&Positional, &args(["a", "b"])).unwrap();
        assert_eq!(sql, "?, ?");
        assert_eq!(vals, vec![Value::Text("a".into()), Value::Text("b".into())]);
    }

    #[test]
    fn a_single_arg_is_the_common_case() {
        let (sql, vals) = build(&Numbered, &arg(21)).unwrap();
        assert_eq!(sql, "$1");
        assert_eq!(vals, vec![Value::I32(21)]);
    }
}
