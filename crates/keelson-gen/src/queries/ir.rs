//! What one analysed query carries: its output columns, its parameters, and
//! the **byte spans of its clauses** — the two faces from one pass.
//!
//! Everything here indexes the query file's text. Nothing is copied out of it,
//! because generated code slices the `include_str!`ed source at exactly these
//! offsets: the row types and the clause-reconstruction code are two readings
//! of one [`Analysis`], never two parses.

use crate::queries::spec::QuerySpec;

/// A byte range in the query file's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Inclusive start.
    pub start: usize,
    /// Exclusive end.
    pub end: usize,
}

impl Span {
    /// The text this span covers.
    pub fn of<'a>(&self, source: &'a str) -> &'a str {
        &source[self.start..self.end]
    }

    /// Whether the span covers nothing.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Which nested field an output column lands in, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nesting {
    /// A plain column on the row struct.
    Flat,
    /// `related__column` — a to-one nested struct field named `related`.
    ToOne(String),
    /// `related.column` — a to-many nested `Vec` field named `related`.
    ToMany(String),
}

/// One column of the result set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputColumn {
    /// The name the engine gives the column — what `Row::take` is keyed on.
    pub name: String,
    /// The Rust field name (the nesting prefix stripped).
    pub field: String,
    /// Where the field lives.
    pub nesting: Nesting,
    /// The Rust type, *without* the `Option` wrapper.
    pub rust_type: String,
    /// Whether the value can be SQL `NULL` — the `Option<T>` decision.
    pub nullable: bool,
    /// Whether [`nullable`](Self::nullable) is true *only* because an outer
    /// join can make the whole row absent (rule N2). A nested to-one group all
    /// of whose columns are in that position becomes `Option<Nested>`, and
    /// inside it each field goes back to [`inner_nullable`](Self::inner_nullable).
    pub outer_join: bool,
    /// The nullability the column would have with no outer join in play — its
    /// DDL nullability, for a plain column reference.
    pub inner_nullable: bool,
    /// Which rule in the nullability decision table decided
    /// [`nullable`](Self::nullable). Tested rule by rule; also emitted as a doc
    /// comment so the generated file explains itself.
    pub rule: &'static str,
}

/// One `$n` / `?n` placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// The 1-based placeholder number.
    pub number: usize,
    /// The generated field name.
    pub name: String,
    /// The Rust type of the value the caller passes.
    pub rust_type: String,
    /// Where the type came from: `context`, `annotation`, `limit`, ….
    pub rule: &'static str,
}

/// A placeholder occurrence in the SQL text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placeholder {
    /// Its byte span in the file.
    pub span: Span,
    /// The 1-based placeholder number.
    pub number: usize,
}

/// The clause spans that make the mod face possible.
///
/// Each is the clause **body** — the text after the keyword — so it can be fed
/// straight into the host statement's corresponding clause. `None` means the
/// query has no such clause.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Clauses {
    /// `SELECT` **`a, b`**. Not contributed to a host that already projects;
    /// see the module docs.
    pub select_list: Option<Span>,
    /// `FROM` **`t JOIN u ON …`** — joins included, which is what keeps the
    /// merged statement flat.
    pub from: Option<Span>,
    /// `WHERE` **`…`**, merged into the host's `WHERE` with `AND`.
    pub where_: Option<Span>,
    /// `GROUP BY` **`…`**.
    pub group_by: Option<Span>,
    /// `HAVING` **`…`**.
    pub having: Option<Span>,
    /// `ORDER BY` **`…`**.
    pub order_by: Option<Span>,
    /// `LIMIT` **`…`**.
    pub limit: Option<Span>,
    /// `OFFSET` **`…`**.
    pub offset: Option<Span>,
    /// Why this query has no mod face, when it has none. A recorded refusal —
    /// the generator never fakes the mod face by nesting the query as a
    /// sub-select.
    pub unsupported: Option<String>,
}

impl Clauses {
    /// Every clause span present, in statement order — what the emitter walks.
    pub fn present(&self) -> Vec<(&'static str, Span)> {
        let mut out = Vec::new();
        for (name, span) in [
            ("from", self.from),
            ("where", self.where_),
            ("group_by", self.group_by),
            ("having", self.having),
            ("order_by", self.order_by),
            ("limit", self.limit),
            ("offset", self.offset),
        ] {
            if let Some(s) = span {
                out.push((name, s));
            }
        }
        out
    }
}

/// One query, analysed: the row shape, the parameters, and the clause map.
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The annotations and the SQL span it came from.
    pub spec: QuerySpec,
    /// The result columns, in select-list order.
    pub outputs: Vec<OutputColumn>,
    /// The parameters, in placeholder order.
    pub params: Vec<Param>,
    /// Every placeholder occurrence, in text order (a number may repeat).
    pub placeholders: Vec<Placeholder>,
    /// The clause spans.
    pub clauses: Clauses,
}

/// One piece of a reconstructed fragment: literal text from the source, or a
/// bound argument in place of a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// A byte range of the query file, written verbatim.
    Sql(Span),
    /// The 0-based index into the query's argument vector.
    Arg(usize),
}

impl Analysis {
    /// Cut `span` into literal text and argument holes.
    ///
    /// This is what makes both faces work: a fragment renders through the
    /// writer's `push_arg`, so its placeholders are **re-numbered by the host
    /// statement** rather than carried over from the file. A placeholder used
    /// twice therefore binds its value twice — semantically identical, and the
    /// only way a slice of foreign text can compose with a builder's counter.
    pub fn parts(&self, span: Span) -> Vec<Part> {
        let mut parts = Vec::new();
        let mut at = span.start;
        for ph in &self.placeholders {
            if ph.span.start < span.start || ph.span.end > span.end {
                continue;
            }
            if ph.span.start > at {
                parts.push(Part::Sql(Span {
                    start: at,
                    end: ph.span.start,
                }));
            }
            let index = self
                .params
                .iter()
                .position(|p| p.number == ph.number)
                .expect("every placeholder has a param (checked during analysis)");
            parts.push(Part::Arg(index));
            at = ph.span.end;
        }
        if at < span.end {
            parts.push(Part::Sql(Span {
                start: at,
                end: span.end,
            }));
        }
        parts
    }

    /// The whole statement as parts — the query face's rendering.
    pub fn statement_parts(&self) -> Vec<Part> {
        self.parts(Span {
            start: self.spec.sql_start,
            end: self.spec.sql_end,
        })
    }
}
