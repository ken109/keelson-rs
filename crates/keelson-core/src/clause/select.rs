use crate::expr::{Expr, IntoExpr, IntoExprList};
use crate::writer::{Expression, SqlWriter};

/// The projection: `SELECT` **`a, b, c`**, or `*` when nothing was asked for.
///
/// Preload columns are kept apart from the ones the caller selected so that a
/// preloader can be an ordinary query mod while the mapper still knows how many of
/// the returned columns belong to the root object — that is what
/// [`count_select_cols`](Self::count_select_cols) is for. They render as one list,
/// preloads last.
///
/// This is the one clause whose "absent" rendering is not empty: a `SELECT` with
/// no list is not a statement, and `*` is what the grammar asks for. Absence is
/// therefore still observable through [`is_empty`](Self::is_empty).
#[derive(Debug, Clone, Default)]
pub struct SelectList {
    /// What the caller selected.
    pub columns: Vec<Expr>,
    /// Columns a preloader added, rendered after [`columns`](Self::columns).
    pub preload_columns: Vec<Expr>,
}

impl SelectList {
    /// How many columns the caller selected, ignoring preloads.
    pub fn count_select_cols(&self) -> usize {
        self.columns.len()
    }

    /// Replace the selected columns.
    pub fn set_select(&mut self, columns: impl IntoExprList) {
        self.columns = columns.into_expr_list();
    }

    /// Add to the selected columns.
    pub fn append_select(&mut self, columns: impl IntoExprList) {
        self.columns.extend(columns.into_expr_list());
    }

    /// Replace the preload columns.
    pub fn set_preload_select(&mut self, columns: impl IntoExprList) {
        self.preload_columns = columns.into_expr_list();
    }

    /// Add to the preload columns.
    pub fn append_preload_select(&mut self, columns: impl IntoExprList) {
        self.preload_columns.extend(columns.into_expr_list());
    }

    /// Add one column.
    pub fn append_column(&mut self, column: impl IntoExpr) {
        self.columns.push(column.into_expr());
    }

    /// Whether nothing was selected, so that `*` is what will be written.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.preload_columns.is_empty()
    }
}

impl Expression for SelectList {
    fn write_sql(&self, w: &mut SqlWriter<'_>) {
        if self.is_empty() {
            w.push_str("*");
            return;
        }
        w.write_iter(
            self.columns.iter().chain(self.preload_columns.iter()),
            "",
            ", ",
            "",
        );
    }
}

/// A statement with a projection.
pub trait HasSelectList {
    /// The projection to modify.
    fn select_list_mut(&mut self) -> &mut SelectList;
}

impl HasSelectList for SelectList {
    fn select_list_mut(&mut self) -> &mut SelectList {
        self
    }
}

#[cfg(test)]
mod tests {
    use keelson_sqlcheck::testing::assert_frag_sql;

    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::expr::{Chain, quote};
    use crate::writer::build;

    fn sql(list: &SelectList) -> String {
        build(&Numbered, list).expect("render").0
    }

    #[test]
    fn an_empty_projection_is_a_star() {
        assert_frag_sql("SELECT {} FROM users", &sql(&SelectList::default()), "*");
        assert!(SelectList::default().is_empty());
    }

    #[test]
    fn preloads_render_after_the_selected_columns() {
        let mut list = SelectList::default();
        list.append_select((quote("age"), quote("name")));
        list.append_preload_select(quote(("posts", "id")));

        // Two tables in the frame so the preload's qualified column resolves; the
        // unqualified ones are chosen to be unambiguous across both.
        assert_frag_sql(
            "SELECT {} FROM users, posts",
            &sql(&list),
            r#""age", "name", "posts"."id""#,
        );
        assert_eq!(
            list.count_select_cols(),
            2,
            "a preload column is not part of the root projection"
        );
    }

    #[test]
    fn preloads_alone_still_suppress_the_star() {
        let mut list = SelectList::default();
        list.set_preload_select(quote(("posts", "id")));
        assert_frag_sql("SELECT {} FROM posts", &sql(&list), r#""posts"."id""#);
        assert!(!list.is_empty());
    }

    #[test]
    fn a_column_can_be_any_expression() {
        let mut list = SelectList::default();
        list.append_column(quote("id"));
        list.append_column(Expr::func("count", "*").as_("n"));
        list.append_column("1 + 1");
        // The GROUP BY is the frame's, not the projection's: a select list mixing
        // an aggregate with a plain column needs one to be legal.
        assert_frag_sql(
            r#"SELECT {} FROM users GROUP BY "id""#,
            &sql(&list),
            r#""id", count(*) AS "n", 1 + 1"#,
        );
    }
}
