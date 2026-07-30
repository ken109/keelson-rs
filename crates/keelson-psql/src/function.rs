use keelson_core::clause::{HasOrderBy, OrderBy, Window};
use keelson_core::expr::x_raw;
use keelson_core::{DynExpr, Expression, Mod, Result, SqlWriter};

use crate::expr::Expr;
use crate::into_expr::Exprs;

/// A function call: `avg(salary)`, `LEAD(created_date, 1, NOW())`.
///
/// Everything PostgreSQL allows to hang off a call lives here — `DISTINCT`, an
/// aggregate's `ORDER BY` and `FILTER`, a `WITHIN GROUP`, an `OVER (…)` window,
/// and the column definition list a `ROWS FROM` item needs. They are set with
/// [`fm`](crate::fm) mods through [`apply`](Self::apply).
#[derive(Debug, Clone, Default)]
pub struct Function {
    /// Rendering stops at an empty name, so a default `Function` is invisible.
    pub name: String,
    pub args: Vec<DynExpr>,

    pub distinct: bool,
    pub within_group: bool,
    pub order_by: OrderBy,
    pub filter: Vec<DynExpr>,
    pub window: Option<Window>,

    /// An alias written *before* the column definitions, unquoted:
    /// `f() AS alias (a INTEGER)`.
    pub alias: String,
    pub columns: Vec<ColumnDef>,
}

impl Function {
    /// `name(args...)`.
    pub fn new(name: impl Into<String>, args: impl Exprs) -> Self {
        Function {
            name: name.into(),
            args: args.into_exprs(),
            ..Function::default()
        }
    }

    /// Apply [`fm`](crate::fm) mods.
    ///
    /// bob writes this as a call — `psql.F("avg", "salary")(fm.Over(...))` — by
    /// returning a function from `F`. Rust has no call syntax to overload, so the
    /// mods come through a method instead, and it consumes and returns `self` so
    /// that it chains.
    pub fn apply<M: Mod<Function>>(mut self, m: M) -> Function {
        m.apply(&mut self);
        self
    }

    /// The call as a chainable [`Expr`], never parenthesised.
    ///
    /// bob's `Function` embeds the operator chain, so `f.Minus(x)` works
    /// directly. Struct embedding has no equivalent here — the chain would have
    /// to borrow the function it is built from — so the conversion is explicit.
    pub fn expr(self) -> Expr {
        x_raw(self)
    }

    /// `OVER (…)`.
    pub fn set_window(&mut self, window: Window) {
        self.window = Some(window);
    }

    /// One entry of the column definition list.
    pub fn append_column(&mut self, name: impl Into<String>, data_type: impl Into<String>) {
        self.columns.push(ColumnDef {
            name: name.into(),
            data_type: data_type.into(),
        });
    }
}

impl HasOrderBy for Function {
    fn order_by_mut(&mut self) -> &mut OrderBy {
        &mut self.order_by
    }
}

impl Expression for Function {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.name.is_empty() {
            return Ok(());
        }

        w.push_str(&self.name);
        w.push_str("(");

        if self.distinct {
            w.push_str("DISTINCT ");
        }

        w.write_slice(&self.args, "", ", ", "")?;

        if !self.within_group {
            w.write_if(!self.order_by.is_empty(), " ", &self.order_by, "")?;
        }
        w.push_str(")");

        if self.within_group {
            w.write_if(
                !self.order_by.is_empty(),
                " WITHIN GROUP (",
                &self.order_by,
                ")",
            )?;
        }

        w.write_slice(&self.filter, " FILTER (WHERE ", " AND ", ")")?;

        if !self.columns.is_empty() || !self.alias.is_empty() {
            w.push_str(" AS ");
        }
        if !self.alias.is_empty() {
            w.push_str(&self.alias);
            w.push_str(" ");
        }
        w.write_slice(&self.columns, "(", ", ", ")")?;

        // No space in front of `OVER`: bob emits `row_number()OVER ()` and the
        // recorded fixtures contain it.
        if let Some(window) = &self.window {
            w.write_if(true, "OVER (", window, ")")?;
        }

        Ok(())
    }
}

/// One `name type` pair of a function's column definition list.
#[derive(Debug, Clone, Default)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: String,
}

impl Expression for ColumnDef {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        w.push_str(&self.name);
        w.push_str(" ");
        w.push_str(&self.data_type);
        Ok(())
    }
}

/// A `FROM` item made of several functions, which PostgreSQL spells
/// `ROWS FROM (…)`.
///
/// A single function needs no wrapper, and does not get one.
#[derive(Debug, Clone, Default)]
pub struct Functions(pub Vec<Function>);

impl Expression for Functions {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        let rows_from = self.0.len() > 1;

        if rows_from {
            w.push_str("ROWS FROM (");
        }
        w.write_slice(&self.0, "", ", ", "")?;
        if rows_from {
            w.push_str(")");
        }

        Ok(())
    }
}
