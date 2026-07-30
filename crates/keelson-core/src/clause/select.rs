use crate::error::Result;
use crate::writer::{DynExpr, Expression, SqlWriter};

/// The projection: `SELECT` **`a, b, c`** or `*` when nothing was asked for.
///
/// Preloaded columns are kept apart from the ones the caller selected so that a
/// preloader can be an ordinary query mod while the mapper still knows how many
/// of the returned columns belong to the root object — that is what
/// [`count_select_cols`](Self::count_select_cols) is for. They render as one
/// list, preloads last.
#[derive(Debug, Clone, Default)]
pub struct SelectList {
    pub columns: Vec<DynExpr>,
    pub preload_columns: Vec<DynExpr>,
}

impl SelectList {
    /// How many columns the caller selected, ignoring preloads.
    pub fn count_select_cols(&self) -> usize {
        self.columns.len()
    }

    pub fn set_select(&mut self, columns: impl IntoIterator<Item = DynExpr>) {
        self.columns = columns.into_iter().collect();
    }

    pub fn set_preload_select(&mut self, columns: impl IntoIterator<Item = DynExpr>) {
        self.preload_columns = columns.into_iter().collect();
    }

    pub fn append_select(&mut self, columns: impl IntoIterator<Item = DynExpr>) {
        self.columns.extend(columns);
    }

    pub fn append_preload_select(&mut self, columns: impl IntoIterator<Item = DynExpr>) {
        self.preload_columns.extend(columns);
    }
}

impl Expression for SelectList {
    fn write_sql(&self, w: &mut SqlWriter<'_>) -> Result<()> {
        if self.columns.is_empty() && self.preload_columns.is_empty() {
            w.push_str("*");
            return Ok(());
        }
        w.write_iter(
            self.columns.iter().chain(self.preload_columns.iter()),
            "",
            ", ",
            "",
        )
    }
}

/// A query with a projection.
pub trait HasSelectList {
    fn select_list_mut(&mut self) -> &mut SelectList;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::testing::Numbered;
    use crate::writer::{build, dyn_expr};

    #[test]
    fn an_empty_projection_is_a_star() {
        let (sql, _) = build(&Numbered, &SelectList::default()).unwrap();
        assert_eq!(sql, "*");
    }

    #[test]
    fn preloads_render_after_the_selected_columns() {
        let mut list = SelectList::default();
        list.append_select([dyn_expr("id"), dyn_expr("name")]);
        list.append_preload_select([dyn_expr("pilot.id")]);

        let (sql, _) = build(&Numbered, &list).unwrap();
        assert_eq!(sql, "id, name, pilot.id");
        assert_eq!(
            list.count_select_cols(),
            2,
            "the preload column is not part of the root projection"
        );
    }

    #[test]
    fn preloads_alone_still_suppress_the_star() {
        let mut list = SelectList::default();
        list.set_preload_select([dyn_expr("pilot.id")]);
        assert_eq!(build(&Numbered, &list).unwrap().0, "pilot.id");
    }
}
