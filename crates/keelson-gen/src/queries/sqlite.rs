//! SQLite inference, over `sqlite3-parser` (`lemon-rs` — SQLite's own
//! `parse.y` and lexer, ported C→Rust), the same parser keelson-sqlcheck's
//! grammar tier judges with.
//!
//! The rules are [`super`](crate::queries)'s decision table, unchanged: the
//! nullability half is a property of the SQL, not of the engine, so N1–N16
//! mean here exactly what they mean for PostgreSQL. What differs is the type
//! half, and only because SQLite's schema carries less: a declared type is a
//! hint about affinity, so `SUM(x)` over an `INTEGER` column is `i64` rather
//! than PostgreSQL's widened `numeric`, and an expression the type table
//! cannot place needs a `-- column:` annotation more often. That is the
//! schema's limit, recorded, not a weaker analyser.

use std::collections::BTreeMap;

use sqlite3_parser::ast;
use sqlite3_parser::{Bump, FallibleIterator, lexer::sql::Parser};

use crate::config::{Config, Dialect};
use crate::error::{GenError, Result};
use crate::queries::ir::{Analysis, OutputColumn};
use crate::queries::lex;
use crate::queries::nest;
use crate::queries::spec::QuerySpec;
use crate::schema::{Schema, TableDef};

/// Placeholder number → (suggested name, Rust type, which rule found it).
type ParamMap = BTreeMap<usize, (String, String, &'static str)>;

/// One `FROM` item and whether a join can make its columns NULL.
#[derive(Debug, Clone)]
struct Source {
    key: String,
    table: Option<TableDef>,
    outer: bool,
}

struct Scope<'a> {
    schema: &'a Schema,
    config: &'a Config,
    spec: &'a QuerySpec,
    sources: Vec<Source>,
}

/// What one expression yielded. The SQLite twin of the psql analyser's.
#[derive(Debug, Clone)]
struct Inferred {
    rust_type: Option<String>,
    nullable: bool,
    outer_join: bool,
    inner_nullable: bool,
    rule: &'static str,
    name: Option<String>,
}

impl Inferred {
    fn new(rust_type: Option<String>, nullable: bool, rule: &'static str) -> Inferred {
        Inferred {
            rust_type,
            nullable,
            outer_join: false,
            inner_nullable: nullable,
            rule,
            name: None,
        }
    }

    fn known(t: &str, nullable: bool, rule: &'static str) -> Inferred {
        Inferred::new(Some(t.to_owned()), nullable, rule)
    }

    fn unknown(rule: &'static str) -> Inferred {
        Inferred::new(None, true, rule)
    }

    fn named(mut self, name: &str) -> Inferred {
        self.name = Some(name.to_owned());
        self
    }
}

/// Analyse one SQLite query.
pub fn analyse(
    schema: &Schema,
    config: &Config,
    spec: &QuerySpec,
    source: &str,
) -> Result<Analysis> {
    let sql = spec.sql(source);
    let tokens = lex::scan_sqlite(sql, spec.sql_start)
        .map_err(|e| GenError::Config(format!("query `{}`: {e}", spec.name)))?;
    let placeholders = lex::placeholders(&tokens);
    let clauses = lex::clauses(&tokens, spec.sql_start, spec.sql_end);

    let arena = Bump::new();
    let mut parser = Parser::new(&arena, sql.as_bytes());
    let cmd = parser
        .next()
        .map_err(|e| GenError::Config(format!("query `{}`: SQLite rejected it: {e}", spec.name)))?
        .ok_or_else(|| GenError::Config(format!("query `{}`: no statement", spec.name)))?;

    let ast::Cmd::Stmt(stmt) = cmd else {
        return Err(GenError::Unsupported(format!(
            "query `{}`: only a statement can be generated from",
            spec.name
        )));
    };

    // A mutation is typed from its `RETURNING` list (empty for `:exec`) and
    // its `WHERE`; it has no mod face, which `lex::clauses` has already said.
    let mutation = match &stmt {
        ast::Stmt::Update {
            tbl_name,
            from,
            where_clause,
            returning,
            ..
        } => Some((tbl_name, from.as_ref(), where_clause.as_ref(), *returning)),
        ast::Stmt::Delete {
            tbl_name,
            where_clause,
            returning,
            ..
        } => Some((tbl_name, None, where_clause.as_ref(), *returning)),
        _ => None,
    };
    if let Some((tbl_name, from, where_clause, returning)) = mutation {
        let mut scope = Scope {
            schema,
            config,
            spec,
            sources: Vec::new(),
        };
        scope.add_named_table(&name(&tbl_name.name), None, &spec.name)?;
        if let Some(from) = from {
            scope.collect_from(from, &spec.name)?;
        }
        let outputs = scope.outputs(returning.unwrap_or(&[]))?;
        let mut found = ParamMap::new();
        if let Some(w) = where_clause {
            scope.walk_params(w, &mut found);
        }
        let params = crate::queries::assemble_params(spec, &placeholders, &found, '?')?;
        return Ok(Analysis {
            spec: spec.clone(),
            outputs,
            params,
            placeholders,
            clauses,
        });
    }

    let ast::Stmt::Select(select) = &stmt else {
        return Err(GenError::Unsupported(format!(
            "query `{}`: {} is not a statement this generator can type",
            spec.name,
            stmt_kind(&stmt)
        )));
    };

    // Rule N14: every arm of a compound select contributes to one row type, so
    // a column nullable in any arm is nullable in the result.
    let (mut outputs, mut found) = one_select(schema, config, spec, &select.body.select)?;
    for compound in select.body.compounds.iter().flat_map(|c| c.iter()) {
        let (right, right_found) = one_select(schema, config, spec, &compound.select)?;
        if outputs.len() != right.len() {
            return Err(GenError::Config(format!(
                "query `{}`: the arms of the compound select return {} and {} columns",
                spec.name,
                outputs.len(),
                right.len()
            )));
        }
        for (l, r) in outputs.iter_mut().zip(&right) {
            if l.rust_type != r.rust_type {
                return Err(GenError::Config(format!(
                    "query `{}`: column `{}` is `{}` in one arm of the compound select and \
                     `{}` in another; cast them to the same type or add `-- column:`",
                    spec.name, l.name, l.rust_type, r.rust_type
                )));
            }
            if r.nullable {
                l.nullable = true;
                l.inner_nullable = true;
                l.outer_join = false;
                l.rule = "N14";
            }
        }
        for (k, v) in right_found {
            found.entry(k).or_insert(v);
        }
    }

    if let Some(limit) = select.limit {
        note_limit(&limit.expr, "limit", &mut found);
        if let Some(offset) = &limit.offset {
            note_limit(offset, "offset", &mut found);
        }
    }
    let params = crate::queries::assemble_params(spec, &placeholders, &found, '?')?;

    Ok(Analysis {
        spec: spec.clone(),
        outputs,
        params,
        placeholders,
        clauses,
    })
}

/// One arm of a (possibly compound) `SELECT`, with its own `FROM` scope.
fn one_select(
    schema: &Schema,
    config: &Config,
    spec: &QuerySpec,
    one: &ast::OneSelect<'_>,
) -> Result<(Vec<OutputColumn>, ParamMap)> {
    let ast::OneSelect::Select {
        columns,
        from,
        where_clause,
        having,
        ..
    } = one
    else {
        return Err(GenError::Unsupported(format!(
            "query `{}`: a bare VALUES has no schema to type against",
            spec.name
        )));
    };
    let mut scope = Scope {
        schema,
        config,
        spec,
        sources: Vec::new(),
    };
    if let Some(from) = from {
        scope.collect_from(from, &spec.name)?;
    }
    let outputs = scope.outputs(columns)?;
    let mut found = ParamMap::new();
    // A placeholder can sit anywhere an expression can, so every expression
    // slot of the arm is walked — not only the `WHERE`.
    for c in *columns {
        if let ast::ResultColumn::Expr(e, _) = c {
            scope.walk_params(e, &mut found);
        }
    }
    if let Some(from) = from {
        for join in from.joins.iter().flat_map(|j| j.iter()) {
            if let Some(ast::JoinConstraint::On(on)) = &join.constraint {
                scope.walk_params(on, &mut found);
            }
        }
    }
    if let Some(w) = where_clause {
        scope.walk_params(w, &mut found);
    }
    if let Some(h) = having {
        scope.walk_params(h, &mut found);
    }
    Ok((outputs, found))
}

fn stmt_kind(stmt: &ast::Stmt<'_>) -> &'static str {
    match stmt {
        ast::Stmt::Insert { .. } => "INSERT",
        ast::Stmt::Update { .. } => "UPDATE",
        ast::Stmt::Delete { .. } => "DELETE",
        _ => "this statement",
    }
}

fn note_limit(expr: &ast::Expr<'_>, what: &'static str, out: &mut ParamMap) {
    if let ast::Expr::Variable(v) = expr
        && let Some(n) = crate::queries::spec::placeholder_number(v)
    {
        out.entry(n)
            .or_insert((what.to_owned(), "i64".to_owned(), "P3"));
    }
}

impl Scope<'_> {
    fn table(&self, name: &str) -> Option<&TableDef> {
        self.schema.tables.iter().find(|t| t.name == name)
    }

    fn collect_from(&mut self, from: &ast::FromClause<'_>, query: &str) -> Result<()> {
        if let Some(first) = from.select {
            self.add_table(first, false, query)?;
        }
        for join in from.joins.iter().flat_map(|j| j.iter()) {
            // A join whose operator carries LEFT (or FULL) makes the right
            // side's columns nullable — rule N2. SQLite has no RIGHT JOIN
            // before 3.39 and spells the rest the same way.
            let outer = matches!(
                join.operator,
                ast::JoinOperator::TypedJoin(Some(t))
                    if t.contains(ast::JoinType::LEFT) || t.contains(ast::JoinType::RIGHT)
            );
            if matches!(
                join.operator,
                ast::JoinOperator::TypedJoin(Some(t)) if t.contains(ast::JoinType::RIGHT)
            ) {
                for s in &mut self.sources {
                    s.outer = true;
                }
            }
            self.add_table(&join.table, outer, query)?;
        }
        Ok(())
    }

    /// Put one introspected table into scope under `alias` (or its own name).
    fn add_named_table(&mut self, relname: &str, alias: Option<String>, query: &str) -> Result<()> {
        let table = self.table(relname).cloned().ok_or_else(|| {
            GenError::Config(format!(
                "query `{query}`: `{relname}` is not a table or view in the introspected schema"
            ))
        })?;
        self.sources.push(Source {
            key: alias.unwrap_or_else(|| relname.to_owned()),
            table: Some(table),
            outer: false,
        });
        Ok(())
    }

    fn add_table(&mut self, t: &ast::SelectTable<'_>, outer: bool, query: &str) -> Result<()> {
        match t {
            ast::SelectTable::Table(qname, alias, _) => {
                let relname = name(&qname.name);
                let table = self.table(&relname).cloned().ok_or_else(|| {
                    GenError::Config(format!(
                        "query `{query}`: `{relname}` is not a table or view in the introspected \
                         schema"
                    ))
                })?;
                let key = alias
                    .as_ref()
                    .map(alias_name)
                    .unwrap_or_else(|| relname.clone());
                self.sources.push(Source {
                    key,
                    table: Some(table),
                    outer,
                });
                Ok(())
            }
            ast::SelectTable::Select(_, alias) | ast::SelectTable::Sub(_, alias) => {
                self.sources.push(Source {
                    key: alias.as_ref().map(alias_name).unwrap_or_default(),
                    table: None,
                    outer,
                });
                Ok(())
            }
            ast::SelectTable::TableCall(..) => Err(GenError::Unsupported(format!(
                "query `{query}`: a table-valued function in FROM cannot be typed"
            ))),
        }
    }

    fn column(&self, qualifier: Option<&str>, name: &str, query: &str) -> Result<Inferred> {
        let candidates: Vec<&Source> = match qualifier {
            Some(q) => self.sources.iter().filter(|s| s.key == q).collect(),
            None => self.sources.iter().collect(),
        };
        if candidates.is_empty() {
            return Err(GenError::Config(format!(
                "query `{query}`: `{}` refers to nothing in FROM",
                qualifier.unwrap_or(name)
            )));
        }
        let mut hits = Vec::new();
        for s in candidates {
            let Some(table) = &s.table else {
                return Err(GenError::Unsupported(format!(
                    "query `{query}`: `{name}` comes from a sub-select the generator does not \
                     look inside; give the column an `-- column:` annotation"
                )));
            };
            if let Some(c) = table.column(name) {
                hits.push((s, table, c));
            }
        }
        match hits.as_slice() {
            [] => Err(GenError::Config(format!(
                "query `{query}`: no column `{name}` in {}",
                self.sources
                    .iter()
                    .map(|s| s.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            [(s, table, c)] => {
                let resolved =
                    crate::typemap::resolve(Dialect::Sqlite, &self.config.types, table, c)?;
                let (nullable, rule) = if s.outer {
                    (true, "N2")
                } else {
                    (c.nullable, "N1")
                };
                Ok(Inferred {
                    rust_type: Some(resolved.rust_type),
                    nullable,
                    outer_join: s.outer,
                    inner_nullable: c.nullable,
                    rule,
                    name: Some(name.to_owned()),
                })
            }
            _ => Err(GenError::Config(format!(
                "query `{query}`: `{name}` is ambiguous — qualify it"
            ))),
        }
    }

    fn outputs(&self, columns: &[ast::ResultColumn<'_>]) -> Result<Vec<OutputColumn>> {
        let query = &self.spec.name;
        let mut out = Vec::new();
        for c in columns {
            match c {
                ast::ResultColumn::Star => {
                    for s in &self.sources {
                        self.expand(s, &mut out)?;
                    }
                }
                ast::ResultColumn::TableStar(id) => {
                    let key = name(id);
                    let s = self.sources.iter().find(|s| s.key == key).ok_or_else(|| {
                        GenError::Config(format!(
                            "query `{query}`: `{key}.*` refers to nothing in FROM"
                        ))
                    })?;
                    self.expand(s, &mut out)?;
                }
                ast::ResultColumn::Expr(e, alias) => {
                    let inferred = self.infer(e)?;
                    let name = match alias {
                        Some(a) => alias_name(a),
                        None => inferred.name.clone().ok_or_else(|| {
                            GenError::Config(format!(
                                "query `{query}`: an output column has no name of its own; add \
                                 an `AS` alias"
                            ))
                        })?,
                    };
                    out.push(self.finish(&name, inferred)?);
                }
            }
        }
        if out.is_empty() && self.spec.cardinality.returns_rows() {
            return Err(GenError::Config(format!(
                "query `{query}`: rows were asked for but the statement returns no columns"
            )));
        }
        Ok(out)
    }

    fn expand(&self, s: &Source, out: &mut Vec<OutputColumn>) -> Result<()> {
        let query = &self.spec.name;
        let Some(table) = &s.table else {
            return Err(GenError::Unsupported(format!(
                "query `{query}`: `*` over a sub-select cannot be expanded; list the columns"
            )));
        };
        for c in &table.columns {
            if !self.config.includes_column(&table.name, &c.name) {
                continue;
            }
            let inferred = self.column(Some(&s.key), &c.name, query)?;
            out.push(self.finish(&c.name, inferred)?);
        }
        Ok(())
    }

    fn finish(&self, name: &str, inferred: Inferred) -> Result<OutputColumn> {
        let query = &self.spec.name;
        let (rust_type, mut rule) = match self.spec.column_types.get(name) {
            Some(t) => (t.clone(), "A1"),
            None => (
                inferred.rust_type.ok_or_else(|| {
                    GenError::Config(format!(
                        "query `{query}`: the type of output column `{name}` cannot be inferred \
                         ({}); add `-- column: {name} <RustType>`",
                        inferred.rule
                    ))
                })?,
                inferred.rule,
            ),
        };
        let mut nullable = inferred.nullable;
        let mut outer_join = inferred.outer_join;
        let mut inner_nullable = inferred.inner_nullable;
        if let Some(n) = self.spec.column_nullable.get(name) {
            nullable = *n;
            inner_nullable = *n;
            outer_join = false;
            rule = "N16";
        }
        let (nesting, field) = nest::split(name, self.spec.prefix.as_deref());
        Ok(OutputColumn {
            name: name.to_owned(),
            field,
            nesting,
            rust_type,
            nullable,
            outer_join,
            inner_nullable,
            rule,
        })
    }

    // --- expressions -------------------------------------------------------

    fn infer(&self, e: &ast::Expr<'_>) -> Result<Inferred> {
        let query = &self.spec.name;
        Ok(match e {
            ast::Expr::Id(id) => self.column(None, &id_name(id), query)?,
            ast::Expr::Name(id) => self.column(None, &name(id), query)?,
            ast::Expr::Qualified(q, id) => self.column(Some(&name(q)), &name(id), query)?,
            ast::Expr::DoublyQualified(_, q, id) => {
                self.column(Some(&name(q)), &name(id), query)?
            }
            ast::Expr::Literal(lit) => literal(lit),
            ast::Expr::Cast { expr, type_name } => {
                let inner = self.infer(expr)?;
                let target = type_name.as_ref().map(|t| t.name.to_string());
                match target
                    .as_deref()
                    .and_then(|n| crate::typemap::cast_target(Dialect::Sqlite, n))
                {
                    Some(t) => Inferred::known(t, inner.nullable, "N13"),
                    None => Inferred::unknown("N13"),
                }
            }
            ast::Expr::Binary(l, op, r) => {
                let (li, ri) = (self.infer(l)?, self.infer(r)?);
                let nullable = li.nullable || ri.nullable;
                match op {
                    ast::Operator::Equals
                    | ast::Operator::NotEquals
                    | ast::Operator::Less
                    | ast::Operator::LessEquals
                    | ast::Operator::Greater
                    | ast::Operator::GreaterEquals => {
                        Inferred::known("i64", nullable, "N10").named("bool")
                    }
                    // Rule N11: `IS` / `IS NOT` is the one comparison that
                    // answers rather than propagating — SQLite spells
                    // `x IS NOT NULL` this way instead of as a null test.
                    ast::Operator::Is | ast::Operator::IsNot => {
                        Inferred::known("i64", false, "N11").named("bool")
                    }
                    ast::Operator::And | ast::Operator::Or => {
                        Inferred::known("i64", nullable, "N10").named("bool")
                    }
                    ast::Operator::Concat => {
                        Inferred::known("String", nullable, "N10").named("concat")
                    }
                    _ => {
                        let ty = li.rust_type.clone().or_else(|| ri.rust_type.clone());
                        Inferred::new(ty, nullable, "N10").named("expr")
                    }
                }
            }
            ast::Expr::Unary(_, inner) => {
                let i = self.infer(inner)?;
                Inferred::new(i.rust_type, i.nullable, "N10").named("expr")
            }
            ast::Expr::Parenthesized(inner) => match inner.first() {
                Some(first) => self.infer(first)?,
                None => Inferred::unknown("U0"),
            },
            ast::Expr::IsNull(_) | ast::Expr::NotNull(_) => {
                Inferred::known("i64", false, "N11").named("bool")
            }
            ast::Expr::Exists(_) => Inferred::known("i64", false, "N11").named("exists"),
            ast::Expr::InList { .. } | ast::Expr::InSelect { .. } | ast::Expr::InTable { .. } => {
                Inferred::known("i64", false, "N11").named("bool")
            }
            ast::Expr::Between { lhs, .. } => {
                let i = self.infer(lhs)?;
                Inferred::known("i64", i.nullable, "N10").named("bool")
            }
            ast::Expr::Like { lhs, .. } => {
                let i = self.infer(lhs)?;
                Inferred::known("i64", i.nullable, "N10").named("bool")
            }
            ast::Expr::Case {
                when_then_pairs,
                else_expr,
                ..
            } => {
                // Rule N9.
                let mut ty: Option<String> = None;
                let mut nullable = else_expr.is_none();
                for (_, then) in when_then_pairs.iter() {
                    let i = self.infer(then)?;
                    if ty.is_none() {
                        ty.clone_from(&i.rust_type);
                    }
                    nullable |= i.nullable;
                }
                if let Some(e) = else_expr {
                    let i = self.infer(e)?;
                    if ty.is_none() {
                        ty = i.rust_type;
                    }
                    nullable |= i.nullable;
                }
                Inferred::new(ty, nullable, "N9").named("case")
            }
            ast::Expr::Subquery(_) => Inferred::unknown("N12"),
            // As in the psql analyser: a bound parameter is a value, not a
            // source of NULL, so rule N10 does not propagate from it.
            ast::Expr::Variable(_) => Inferred::new(None, false, "P0"),
            ast::Expr::FunctionCall {
                name: fname, args, ..
            } => self.infer_call(&unquote(fname.0), args.unwrap_or(&[]))?,
            ast::Expr::FunctionCallStar { name: fname, .. } => {
                self.infer_call(&unquote(fname.0), &[])?
            }
            _ => Inferred::unknown("U0"),
        })
    }

    fn infer_call(&self, name: &str, args: &[ast::Expr<'_>]) -> Result<Inferred> {
        let lower = name.to_ascii_lowercase();
        let first = match args.first() {
            Some(a) => Some(self.infer(a)?),
            None => None,
        };
        let arg_type = first.as_ref().and_then(|i| i.rust_type.clone());
        let arg_nullable = first.as_ref().is_some_and(|i| i.nullable);

        Ok(match lower.as_str() {
            // Rule N4.
            "count" => Inferred::known("i64", false, "N4").named("count"),
            // Rule N7: SQLite's coalesce/ifnull.
            "coalesce" | "ifnull" => {
                let mut ty = None;
                let mut nullable = true;
                for a in args {
                    let i = self.infer(a)?;
                    if ty.is_none() {
                        ty.clone_from(&i.rust_type);
                    }
                    nullable &= i.nullable;
                }
                Inferred::new(ty, nullable, "N7").named("coalesce")
            }
            // Rule N5. SQLite does not widen: sum over INTEGER stays integer
            // unless a REAL turns up, and total() is REAL always.
            "sum" => Inferred::new(arg_type.or(Some("i64".to_owned())), true, "N5").named(&lower),
            "total" => Inferred::known("f64", false, "N5").named(&lower),
            "avg" => Inferred::known("f64", true, "N5").named(&lower),
            "min" | "max" => Inferred::new(arg_type, true, "N5").named(&lower),
            "group_concat" => Inferred::known("String", true, "N5").named(&lower),
            // Scalar functions.
            "lower" | "upper" | "trim" | "ltrim" | "rtrim" | "replace" | "substr" | "hex" => {
                Inferred::new(Some("String".to_owned()), arg_nullable, "N10").named(&lower)
            }
            "length" => Inferred::new(Some("i64".to_owned()), arg_nullable, "N10").named(&lower),
            "abs" | "round" => Inferred::new(arg_type, arg_nullable, "N10").named(&lower),
            "datetime" | "current_timestamp" => {
                Inferred::known("chrono::NaiveDateTime", false, "N8").named(&lower)
            }
            "date" | "current_date" => {
                Inferred::known("chrono::NaiveDate", false, "N8").named(&lower)
            }
            "row_number" | "rank" | "dense_rank" | "ntile" => {
                Inferred::known("i64", false, "N15").named(&lower)
            }
            _ => Inferred::new(None, true, "U1").named(&lower),
        })
    }

    // --- parameters --------------------------------------------------------

    fn walk_params(&self, e: &ast::Expr<'_>, out: &mut ParamMap) {
        match e {
            ast::Expr::Binary(l, _, r) => {
                self.pair(l, r, out);
                self.pair(r, l, out);
                self.walk_params(l, out);
                self.walk_params(r, out);
            }
            ast::Expr::Unary(_, inner)
            | ast::Expr::IsNull(inner)
            | ast::Expr::NotNull(inner)
            | ast::Expr::Cast { expr: inner, .. } => self.walk_params(inner, out),
            ast::Expr::Parenthesized(items) => {
                for i in items.iter() {
                    self.walk_params(i, out);
                }
            }
            ast::Expr::InList { lhs, rhs, .. } => {
                for r in rhs.iter().flat_map(|r| r.iter()) {
                    self.pair(lhs, r, out);
                }
                self.walk_params(lhs, out);
            }
            ast::Expr::Between {
                lhs, start, end, ..
            } => {
                self.pair(lhs, start, out);
                self.pair(lhs, end, out);
                self.walk_params(lhs, out);
            }
            ast::Expr::Like { lhs, rhs, .. } => {
                self.pair(lhs, rhs, out);
                self.walk_params(lhs, out);
            }
            ast::Expr::Case {
                when_then_pairs,
                else_expr,
                ..
            } => {
                for (w, t) in when_then_pairs.iter() {
                    self.walk_params(w, out);
                    self.walk_params(t, out);
                }
                if let Some(e) = else_expr {
                    self.walk_params(e, out);
                }
            }
            ast::Expr::FunctionCall { args, .. } => {
                for a in args.iter().flat_map(|a| a.iter()) {
                    self.walk_params(a, out);
                }
            }
            _ => {}
        }
    }

    /// `known <op> ?n` types `?n`.
    fn pair(&self, known: &ast::Expr<'_>, param: &ast::Expr<'_>, out: &mut ParamMap) {
        let ast::Expr::Variable(v) = param else {
            return;
        };
        let Some(n) = crate::queries::spec::placeholder_number(v) else {
            return;
        };
        let Ok(i) = self.infer(known) else { return };
        let Some(ty) = i.rust_type else { return };
        let name = i.name.unwrap_or_else(|| "arg".to_owned());
        out.entry(n).or_insert((name, ty, "P1"));
    }
}

fn alias_name(a: &ast::As<'_>) -> String {
    match a {
        ast::As::As(n) | ast::As::Elided(n) => name(n),
    }
}

/// A `Name` as SQLite spells it, with its delimiters taken back off.
///
/// `lemon-rs` keeps an identifier's quoting in the token, so `AS "tags.name"`
/// arrives as `"tags.name"` — quotes and all. The name the *engine* reports
/// for that column is `tags.name`, which is what a row is keyed on and what
/// the nested-row naming splits, so the delimiters come off here, once.
fn name(n: &ast::Name<'_>) -> String {
    unquote(n.0)
}

/// The same, for the `Id` spelling `lemon-rs` uses in expressions.
fn id_name(n: &ast::Id<'_>) -> String {
    unquote(n.0)
}

/// Strip one layer of SQLite identifier quoting, undoubling an embedded
/// delimiter. SQLite accepts four spellings; all four mean the same name.
fn unquote(raw: &str) -> String {
    for (open, close) in [('"', '"'), ('`', '`'), ('\'', '\''), ('[', ']')] {
        if raw.len() >= 2 && raw.starts_with(open) && raw.ends_with(close) {
            let inner = &raw[open.len_utf8()..raw.len() - close.len_utf8()];
            return if open == close {
                inner.replace(&format!("{open}{open}"), &open.to_string())
            } else {
                inner.to_owned()
            };
        }
    }
    raw.to_owned()
}

/// Rule N8 for SQLite's literal forms.
fn literal(lit: &ast::Literal<'_>) -> Inferred {
    match lit {
        ast::Literal::Numeric(n) => {
            if n.contains('.') || n.contains('e') || n.contains('E') {
                Inferred::known("f64", false, "N8").named("real")
            } else {
                Inferred::known("i64", false, "N8").named("int")
            }
        }
        ast::Literal::String(_) => Inferred::known("String", false, "N8").named("text"),
        ast::Literal::Blob(_) => Inferred::known("Vec<u8>", false, "N8").named("blob"),
        ast::Literal::Keyword(k) => match k.to_ascii_lowercase().as_str() {
            "true" | "false" => Inferred::known("bool", false, "N8").named("bool"),
            "null" => Inferred::new(None, true, "N8").named("null"),
            _ => Inferred::unknown("N8"),
        },
        ast::Literal::Null => Inferred::new(None, true, "N8").named("null"),
        ast::Literal::CurrentDate => {
            Inferred::known("chrono::NaiveDate", false, "N8").named("current_date")
        }
        ast::Literal::CurrentTime => {
            Inferred::known("chrono::NaiveTime", false, "N8").named("current_time")
        }
        ast::Literal::CurrentTimestamp => {
            Inferred::known("chrono::NaiveDateTime", false, "N8").named("current_timestamp")
        }
    }
}
