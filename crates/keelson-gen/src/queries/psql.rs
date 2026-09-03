//! PostgreSQL inference: `pg_query`'s parse tree — PostgreSQL's own — read
//! against the introspected schema.
//!
//! The tree is walked as `serde_json` rather than through the generated prost
//! types, the same choice `keelson-sqlcheck`'s Tier D gate made and for the
//! same reason: the node set is enormous, the walk is uniform, and a field
//! this module does not know about costs nothing.
//!
//! Every rule this module applies is in the decision table in
//! [`super`](crate::queries)'s docs, and each carries the rule's id into
//! [`OutputColumn::rule`] so the tests can assert *which* rule fired rather
//! than only its answer.

use std::collections::BTreeMap;

use serde_json::Value as Json;

use crate::config::{Config, Dialect};
use crate::error::{GenError, Result};
use crate::queries::infer::{Inferred, Source};
use crate::queries::ir::{Analysis, OutputColumn};
use crate::queries::lex;
use crate::queries::nest;
use crate::queries::spec::QuerySpec;
use crate::schema::{Schema, TableDef};

type Obj = serde_json::Map<String, Json>;

/// Placeholder number → (suggested name, Rust type, which rule found it).
type ParamMap = BTreeMap<usize, (String, String, &'static str)>;

// --- JSON accessors, as in keelson-sqlcheck's coverage walker --------------

fn int(node: &Obj, key: &str) -> i64 {
    node.get(key).and_then(Json::as_i64).unwrap_or(0)
}

fn boolean(node: &Obj, key: &str) -> bool {
    node.get(key).and_then(Json::as_bool).unwrap_or(false)
}

fn text<'a>(node: &'a Obj, key: &str) -> &'a str {
    node.get(key).and_then(Json::as_str).unwrap_or("")
}

fn list<'a>(node: &'a Obj, key: &str) -> &'a [Json] {
    node.get(key).and_then(Json::as_array).map_or(&[], |v| v)
}

fn object<'a>(node: &'a Obj, key: &str) -> Option<&'a Obj> {
    node.get(key).and_then(Json::as_object)
}

/// A list element is a `Node` message — `{"node": {"String": {...}}}` — so the
/// variant map sits one `node` layer down.
fn variants(element: &Json) -> Option<&Obj> {
    let obj = element.as_object()?;
    match obj.get("node") {
        Some(inner) => inner.as_object(),
        None => Some(obj),
    }
}

/// The single `{"Kind": {...}}` pair a `Node` carries.
fn one_node(element: &Json) -> Option<(&str, &Obj)> {
    let v = variants(element)?;
    let (key, value) = v.iter().next()?;
    Some((key.as_str(), value.as_object()?))
}

/// The `Node` under `key`, unwrapped to `(kind, fields)`.
fn child<'a>(node: &'a Obj, key: &str) -> Option<(&'a str, &'a Obj)> {
    one_node(node.get(key)?)
}

fn svals(nodes: &[Json]) -> impl DoubleEndedIterator<Item = &str> {
    nodes
        .iter()
        .filter_map(variants)
        .filter_map(|n| n.get("String"))
        .filter_map(Json::as_object)
        .filter_map(|s| s.get("sval"))
        .filter_map(Json::as_str)
}

// --- what a FROM item contributes -----------------------------------------

/// Analyse one PostgreSQL query.
pub fn analyse(
    schema: &Schema,
    config: &Config,
    spec: &QuerySpec,
    source: &str,
) -> Result<Analysis> {
    let sql = spec.sql(source);
    let tokens = lex::scan_psql(sql, spec.sql_start).map_err(|e| {
        GenError::Config(format!(
            "query `{}`: PostgreSQL rejected it: {e}",
            spec.name
        ))
    })?;
    let placeholders = lex::placeholders(&tokens);
    let clauses = lex::clauses(&tokens, spec.sql_start, spec.sql_end);

    let parsed = pg_query::parse(sql).map_err(|e| {
        GenError::Config(format!(
            "query `{}`: PostgreSQL rejected it: {e}",
            spec.name
        ))
    })?;
    let tree = serde_json::to_value(&parsed.protobuf)
        .map_err(|e| GenError::Config(format!("query `{}`: parse tree: {e}", spec.name)))?;
    let root = tree
        .as_object()
        .and_then(|o| o.get("stmts"))
        .and_then(Json::as_array)
        .and_then(|s| s.first())
        .and_then(Json::as_object)
        .and_then(|s| s.get("stmt"))
        .and_then(one_node)
        .ok_or_else(|| {
            GenError::Config(format!("query `{}`: no statement in the file", spec.name))
        })?;

    let (kind, stmt) = root;
    let (outputs, found) = match kind {
        "SelectStmt" => branch(schema, config, spec, stmt)?,
        "InsertStmt" | "UpdateStmt" | "DeleteStmt" => {
            let mut scope = Scope {
                schema,
                config,
                sources: Vec::new(),
                spec,
            };
            scope.add_range_var_node(stmt.get("relation"), false, &spec.name)?;
            if kind != "InsertStmt" {
                scope.collect_from(list(stmt, "from_clause"), false, &spec.name)?;
            }
            let outputs = scope.outputs(list(stmt, "returning_list"))?;
            let mut found = ParamMap::new();
            scope.walk_params(&Json::Object(stmt.clone()), None, &mut found)?;
            (outputs, found)
        }
        other => {
            return Err(GenError::Unsupported(format!(
                "query `{}`: {other} is not a statement this generator can type",
                spec.name
            )));
        }
    };

    let params = crate::queries::assemble_params(spec, &placeholders, &found, '$')?;

    Ok(Analysis {
        spec: spec.clone(),
        outputs,
        params,
        placeholders,
        clauses,
    })
}

/// One arm of a `SELECT`, or the whole of a set operation.
///
/// Rule N14 lives here: `UNION`/`INTERSECT`/`EXCEPT` yields one row type, so
/// the branches are analysed independently and merged column by column — a
/// column nullable in **any** branch is nullable in the result. The types must
/// agree, and a disagreement is a named error rather than a silent pick.
fn branch(
    schema: &Schema,
    config: &Config,
    spec: &QuerySpec,
    stmt: &Obj,
) -> Result<(Vec<OutputColumn>, ParamMap)> {
    // `larg`/`rarg` are bare `SelectStmt` messages, not `Node`s — the one
    // place in this tree where a child is not wrapped.
    if let (Some(larg), Some(rarg)) = (object(stmt, "larg"), object(stmt, "rarg")) {
        let (mut left, mut found) = branch(schema, config, spec, larg)?;
        let (right, right_found) = branch(schema, config, spec, rarg)?;
        if left.len() != right.len() {
            return Err(GenError::Config(format!(
                "query `{}`: the branches of the set operation select {} and {} columns",
                spec.name,
                left.len(),
                right.len()
            )));
        }
        for (l, r) in left.iter_mut().zip(&right) {
            if l.rust_type != r.rust_type {
                return Err(GenError::Config(format!(
                    "query `{}`: column `{}` is `{}` in one branch of the set operation and \
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
        note_limits(stmt, &mut found);
        return Ok((left, found));
    }

    let mut scope = Scope {
        schema,
        config,
        sources: Vec::new(),
        spec,
    };
    scope.collect_from(list(stmt, "from_clause"), false, &spec.name)?;
    let outputs = scope.outputs(list(stmt, "target_list"))?;
    let mut found = ParamMap::new();
    scope.walk_params(&Json::Object(stmt.clone()), None, &mut found)?;
    Ok((outputs, found))
}

/// What one query can refer to, and what the spec says about it.
#[derive(Debug)]
struct Scope<'a> {
    schema: &'a Schema,
    config: &'a Config,
    sources: Vec<Source>,
    /// `-- column:` / `-- nullable:` overrides for this query.
    spec: &'a QuerySpec,
}

impl Scope<'_> {
    fn table(&self, name: &str) -> Option<&TableDef> {
        self.schema.tables.iter().find(|t| t.name == name)
    }

    fn add_range_var_node(&mut self, node: Option<&Json>, outer: bool, query: &str) -> Result<()> {
        let Some(node) = node else { return Ok(()) };
        // `relation` is a bare RangeVar message, not a Node.
        let obj = node
            .as_object()
            .ok_or_else(|| GenError::Config(format!("query `{query}`: unreadable relation")))?;
        self.add_range_var(obj, outer, query)
    }

    fn add_range_var(&mut self, rv: &Obj, outer: bool, query: &str) -> Result<()> {
        let relname = text(rv, "relname");
        let alias = object(rv, "alias")
            .map(|a| text(a, "aliasname").to_owned())
            .filter(|a| !a.is_empty());
        let table = self.table(relname).cloned();
        if table.is_none() {
            return Err(GenError::Config(format!(
                "query `{query}`: `{relname}` is not a table or view in the introspected schema"
            )));
        }
        self.sources.push(Source {
            key: alias.unwrap_or_else(|| relname.to_owned()),
            table,
            outer,
        });
        Ok(())
    }

    /// Walk the `FROM` tree, marking outer-joined sides nullable (rule N2).
    fn collect_from(&mut self, items: &[Json], outer: bool, query: &str) -> Result<()> {
        for item in items {
            let Some((kind, node)) = one_node(item) else {
                continue;
            };
            self.collect_item(kind, node, outer, query)?;
        }
        Ok(())
    }

    fn collect_item(&mut self, kind: &str, node: &Obj, outer: bool, query: &str) -> Result<()> {
        match kind {
            "RangeVar" => self.add_range_var(node, outer, query),
            "JoinExpr" => {
                // JoinType: 1 INNER, 2 LEFT, 3 FULL, 4 RIGHT.
                let jt = int(node, "jointype");
                let (left_outer, right_outer) = match jt {
                    2 => (outer, true),
                    3 => (true, true),
                    4 => (true, outer),
                    _ => (outer, outer),
                };
                if let Some((k, n)) = child(node, "larg") {
                    self.collect_item(k, n, left_outer, query)?;
                }
                if let Some((k, n)) = child(node, "rarg") {
                    self.collect_item(k, n, right_outer, query)?;
                }
                Ok(())
            }
            "RangeSubselect" => {
                let alias = object(node, "alias")
                    .map(|a| text(a, "aliasname").to_owned())
                    .unwrap_or_default();
                self.sources.push(Source {
                    key: alias,
                    table: None,
                    outer,
                });
                Ok(())
            }
            other => Err(GenError::Unsupported(format!(
                "query `{query}`: `{other}` in FROM is not something the generator can type; \
                 annotate the output columns with `-- column:` if you need it"
            ))),
        }
    }

    /// Resolve `qualifier.column` (or a bare `column`) against the scope.
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
                    crate::typemap::resolve(Dialect::Psql, &self.config.types, table, c)?;
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
                "query `{query}`: `{name}` is ambiguous across {} — qualify it",
                hits.iter()
                    .map(|(s, _, _)| s.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// The select list (or `RETURNING` list) → output columns.
    fn outputs(&self, items: &[Json]) -> Result<Vec<OutputColumn>> {
        let query = &self.spec.name;
        let mut out: Vec<OutputColumn> = Vec::new();
        for item in items {
            let Some(("ResTarget", target)) = one_node(item) else {
                continue;
            };
            let alias = text(target, "name");
            let Some((kind, node)) = child(target, "val") else {
                continue;
            };

            // `*` and `t.*` expand from the schema, in catalog order.
            if kind == "ColumnRef"
                && let Some((qualifier, star)) = column_ref_parts(node)
                && star
            {
                if !alias.is_empty() {
                    return Err(GenError::Config(format!(
                        "query `{query}`: `*` cannot carry an alias"
                    )));
                }
                for s in &self.sources {
                    if let Some(q) = &qualifier
                        && s.key != *q
                    {
                        continue;
                    }
                    let Some(table) = &s.table else {
                        return Err(GenError::Unsupported(format!(
                            "query `{query}`: `*` over a sub-select cannot be expanded; \
                             list the columns"
                        )));
                    };
                    for c in &table.columns {
                        if !self.config.includes_column(&table.name, &c.name) {
                            continue;
                        }
                        let inferred = self.column(Some(&s.key), &c.name, query)?;
                        out.push(self.finish(&c.name, inferred)?);
                    }
                }
                continue;
            }

            let inferred = self.infer(kind, node)?;
            let name = if !alias.is_empty() {
                alias.to_owned()
            } else {
                inferred.name.clone().ok_or_else(|| {
                    GenError::Config(format!(
                        "query `{query}`: an output column has no name PostgreSQL would give it; \
                         add an `AS` alias"
                    ))
                })?
            };
            out.push(self.finish(&name, inferred)?);
        }
        if out.is_empty() && self.spec.cardinality.returns_rows() {
            return Err(GenError::Config(format!(
                "query `{query}`: `:{:?}` was asked for but the statement returns no columns",
                self.spec.cardinality
            )));
        }
        Ok(out)
    }

    /// Apply the annotations and the nested-row naming to one inferred column.
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

    // --- expression inference ---------------------------------------------

    fn infer(&self, kind: &str, node: &Obj) -> Result<Inferred> {
        let query = &self.spec.name;
        Ok(match kind {
            "ColumnRef" => {
                let Some((qualifier, star)) = column_ref_parts(node) else {
                    return Err(GenError::Config(format!(
                        "query `{query}`: unreadable column reference"
                    )));
                };
                if star {
                    return Err(GenError::Config(format!(
                        "query `{query}`: `*` is only allowed as a whole select-list item"
                    )));
                }
                let fields: Vec<&str> = svals(list(node, "fields")).collect();
                let (q, name) = match fields.as_slice() {
                    [name] => (None, *name),
                    [q, name] => (Some(*q), *name),
                    [_, q, name] => (Some(*q), *name),
                    _ => {
                        return Err(GenError::Config(format!(
                            "query `{query}`: unreadable column reference"
                        )));
                    }
                };
                let _ = qualifier;
                self.column(q, name, query)?
            }
            "AConst" => const_type(node),
            // A placeholder binds a value the caller supplies, and the
            // generated parameter type is not an `Option` unless a
            // `-- param:` annotation makes it one — so rule N10 does not
            // treat it as a source of NULL.
            "ParamRef" => Inferred::new(None, false, "P0"),
            "TypeCast" => {
                // Rule N13: the cast fixes the type; nullability rides along.
                let target = type_name(node);
                let inner = match child(node, "arg") {
                    Some((k, n)) => self.infer(k, n)?,
                    None => Inferred::unknown("N13"),
                };
                match target
                    .as_deref()
                    .and_then(|n| crate::typemap::cast_target(Dialect::Psql, n))
                {
                    Some(t) => Inferred::known(t, inner.nullable, "N13"),
                    None => Inferred::unknown("N13"),
                }
            }
            "CoalesceExpr" => {
                // Rule N7: NULL only when every argument can be NULL.
                let args = list(node, "args");
                let mut ty = None;
                let mut nullable = true;
                for a in args {
                    let Some((k, n)) = one_node(a) else { continue };
                    let i = self.infer(k, n)?;
                    if ty.is_none() {
                        ty.clone_from(&i.rust_type);
                    }
                    if !i.nullable {
                        nullable = false;
                    }
                }
                Inferred::new(ty, nullable, "N7").named("coalesce")
            }
            "CaseExpr" => {
                // Rule N9: nullable if any arm is, or if there is no ELSE.
                let mut ty: Option<String> = None;
                let mut nullable = child(node, "defresult").is_none();
                for w in list(node, "args") {
                    let Some(("CaseWhen", when)) = one_node(w) else {
                        continue;
                    };
                    if let Some((k, n)) = child(when, "result") {
                        let i = self.infer(k, n)?;
                        if ty.is_none() {
                            ty.clone_from(&i.rust_type);
                        }
                        nullable |= i.nullable;
                    }
                }
                if let Some((k, n)) = child(node, "defresult") {
                    let i = self.infer(k, n)?;
                    if ty.is_none() {
                        ty = i.rust_type;
                    }
                    nullable |= i.nullable;
                }
                Inferred::new(ty, nullable, "N9").named("case")
            }
            "NullTest" | "BooleanTest" => Inferred::known("bool", false, "N11").named("bool"),
            "BoolExpr" => {
                // Rule N10 in its boolean shape.
                let mut nullable = false;
                for a in list(node, "args") {
                    let Some((k, n)) = one_node(a) else { continue };
                    nullable |= self.infer(k, n)?.nullable;
                }
                Inferred::known("bool", nullable, "N10").named("bool")
            }
            "AExpr" => self.infer_a_expr(node)?,
            "SubLink" => {
                // Rule N12: a scalar sub-query yields NULL when it matches no
                // row; EXISTS is a plain boolean (rule N11).
                // SubLinkType (primnodes.h): EXISTS = 1, ALL = 2, ANY = 3 —
                // all three are predicates; EXPR = 5 is the scalar one.
                match int(node, "sub_link_type") {
                    1..=3 => Inferred::known("bool", false, "N11").named("exists"),
                    _ => Inferred::unknown("N12"),
                }
            }
            "FuncCall" => self.infer_func(node)?,
            "SqlvalueFunction" => sql_value_function(int(node, "op")),
            _ => Inferred::unknown("U0"),
        })
    }

    fn infer_a_expr(&self, node: &Obj) -> Result<Inferred> {
        let op = svals(list(node, "name")).next().unwrap_or("").to_owned();
        let mut nullable = false;
        let mut operand: Option<Inferred> = None;
        for side in ["lexpr", "rexpr"] {
            if let Some((k, n)) = child(node, side) {
                let i = self.infer(k, n)?;
                nullable |= i.nullable;
                if operand.is_none() && i.rust_type.is_some() {
                    operand = Some(i);
                }
            }
        }
        // Comparisons and pattern matches are boolean; arithmetic and `||`
        // keep an operand's type. Either way rule N10 propagates NULL.
        Ok(match op.as_str() {
            "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=" | "~~" | "!~~" | "~~*" | "!~~*" | "~"
            | "!~" | "@>" | "<@" => Inferred::known("bool", nullable, "N10").named("bool"),
            "||" => Inferred::known("String", nullable, "N10").named("concat"),
            _ => match operand {
                Some(i) => Inferred::new(i.rust_type, nullable, "N10").named("expr"),
                None => Inferred::unknown("N10"),
            },
        })
    }

    fn infer_func(&self, node: &Obj) -> Result<Inferred> {
        let name = svals(list(node, "funcname"))
            .next_back()
            .unwrap_or("")
            .to_ascii_lowercase();
        let star = boolean(node, "agg_star");
        let args = list(node, "args");
        let first = match args.first().and_then(one_node) {
            Some((k, n)) => Some(self.infer(k, n)?),
            None => None,
        };
        let arg_nullable = first.as_ref().is_some_and(|i| i.nullable);
        let arg_type = first.as_ref().and_then(|i| i.rust_type.clone());
        let windowed = node.get("over").is_some_and(|v| !v.is_null());

        // Rule N4: COUNT never yields NULL — an empty group counts zero.
        if name == "count" {
            let _ = star;
            return Ok(Inferred::known("i64", false, "N4").named("count"));
        }
        // Rule N15: the ranking window functions are always defined.
        if windowed
            && matches!(
                name.as_str(),
                "row_number" | "rank" | "dense_rank" | "ntile"
            )
        {
            return Ok(Inferred::known("i64", false, "N15").named(&name));
        }
        if windowed {
            return Ok(Inferred::new(arg_type, true, "N15").named(&name));
        }
        // Rule N5: every other aggregate is NULL over an empty group.
        Ok(match name.as_str() {
            "sum" => Inferred::new(Some(widen_sum(arg_type.as_deref())), true, "N5").named(&name),
            "avg" => Inferred::new(Some(widen_avg(arg_type.as_deref())), true, "N5").named(&name),
            "min" | "max" => Inferred::new(arg_type, true, "N5").named(&name),
            "string_agg" => Inferred::known("String", true, "N5").named(&name),
            "bool_and" | "bool_or" | "every" => Inferred::known("bool", true, "N5").named(&name),
            // Scalar functions: the result is NULL when the input is.
            "lower" | "upper" | "trim" | "btrim" | "ltrim" | "rtrim" | "substr" | "substring"
            | "replace" | "md5" => {
                Inferred::new(Some("String".to_owned()), arg_nullable, "N10").named(&name)
            }
            // `concat` ignores NULL inputs and returns '' for all of them.
            "concat" | "concat_ws" => Inferred::known("String", false, "N8").named(&name),
            "length" | "char_length" | "character_length" => {
                Inferred::new(Some("i32".to_owned()), arg_nullable, "N10").named(&name)
            }
            "abs" | "round" | "ceil" | "floor" => {
                Inferred::new(arg_type, arg_nullable, "N10").named(&name)
            }
            "greatest" | "least" => Inferred::new(arg_type, arg_nullable, "N10").named(&name),
            "now" | "current_timestamp" | "transaction_timestamp" | "statement_timestamp" => {
                Inferred::known("chrono::DateTime<chrono::Utc>", false, "N8").named(&name)
            }
            "gen_random_uuid" => Inferred::known("uuid::Uuid", false, "N8").named(&name),
            _ => Inferred::new(None, true, "U1").named(&name),
        })
    }

    // --- parameters --------------------------------------------------------

    /// Depth-first over the whole statement, remembering what each `$n` was
    /// weighed against.
    fn walk_params(
        &self,
        value: &Json,
        clause: Option<&'static str>,
        out: &mut ParamMap,
    ) -> Result<()> {
        match value {
            Json::Object(map) => {
                if let Some((kind, node)) = one_node(value) {
                    match kind {
                        "AExpr" => {
                            self.pair_param(node, out)?;
                        }
                        "TypeCast" => {
                            if let Some(("ParamRef", p)) = child(node, "arg")
                                && let Some(t) = type_name(node)
                                    .as_deref()
                                    .and_then(|n| crate::typemap::cast_target(Dialect::Psql, n))
                            {
                                let n = int(p, "number") as usize;
                                out.entry(n)
                                    .or_insert_with(|| (format!("arg{n}"), t.to_owned(), "P2"));
                            }
                        }
                        _ => {}
                    }
                }
                for (key, val) in map {
                    let next = match key.as_str() {
                        "limit_count" => Some("limit"),
                        "limit_offset" => Some("offset"),
                        _ => clause,
                    };
                    if let Some(("ParamRef", p)) = one_node(val)
                        && matches!(next, Some("limit") | Some("offset"))
                    {
                        let n = int(p, "number") as usize;
                        let name = next.expect("matched above").to_owned();
                        out.entry(n).or_insert((name, "i64".to_owned(), "P3"));
                    }
                    self.walk_params(val, next, out)?;
                }
            }
            Json::Array(items) => {
                for item in items {
                    self.walk_params(item, clause, out)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `col <op> $n` (either way round, and `col IN ($1, $2)`) types `$n`.
    fn pair_param(&self, node: &Obj, out: &mut ParamMap) -> Result<()> {
        let sides = [("lexpr", "rexpr"), ("rexpr", "lexpr")];
        for (other, param_side) in sides {
            let Some((ok, on)) = child(node, other) else {
                continue;
            };
            if ok == "ParamRef" {
                continue;
            }
            let Ok(inferred) = self.infer(ok, on) else {
                continue;
            };
            let Some(ty) = inferred.rust_type else {
                continue;
            };
            let name = inferred.name.unwrap_or_else(|| "arg".to_owned());
            let mut record = |p: &Obj| {
                let n = int(p, "number") as usize;
                out.entry(n).or_insert((name.clone(), ty.clone(), "P1"));
            };
            match node.get(param_side).and_then(one_node) {
                Some(("ParamRef", p)) => record(p),
                Some(("List", l)) => {
                    for item in list(l, "items") {
                        if let Some(("ParamRef", p)) = one_node(item) {
                            record(p);
                        }
                    }
                }
                _ => {}
            }
            // `rexpr` of an `IN` is a bare array of Nodes, not a List node.
            if let Some(Json::Array(items)) = node.get(param_side) {
                for item in items {
                    if let Some(("ParamRef", p)) = one_node(item) {
                        record(p);
                    }
                }
            }
        }
        Ok(())
    }
}

/// A `LIMIT $n` / `OFFSET $n` on the *combination* of a set operation, which
/// belongs to no branch's scope.
fn note_limits(stmt: &Obj, found: &mut ParamMap) {
    for (key, what) in [("limit_count", "limit"), ("limit_offset", "offset")] {
        if let Some(("ParamRef", p)) = child(stmt, key) {
            let n = int(p, "number") as usize;
            found
                .entry(n)
                .or_insert((what.to_owned(), "i64".to_owned(), "P3"));
        }
    }
}

/// `(qualifier, is_star)` for a `ColumnRef`.
fn column_ref_parts(node: &Obj) -> Option<(Option<String>, bool)> {
    let fields = list(node, "fields");
    let star = fields
        .iter()
        .filter_map(variants)
        .any(|v| v.contains_key("AStar"));
    let names: Vec<&str> = svals(fields).collect();
    let qualifier = if star && !names.is_empty() {
        Some(names[names.len() - 1].to_owned())
    } else if !star && names.len() >= 2 {
        Some(names[names.len() - 2].to_owned())
    } else {
        None
    };
    Some((qualifier, star))
}

/// Rule N8: a literal's type, and `NULL` as the one nullable literal.
fn const_type(node: &Obj) -> Inferred {
    if boolean(node, "isnull") {
        return Inferred::new(None, true, "N8").named("null");
    }
    // The value rides in `val` as a one-key wrapper: `{"Ival": {"ival": 1}}`.
    match object(node, "val")
        .and_then(|v| v.keys().next())
        .map(String::as_str)
    {
        Some("Ival") => Inferred::known("i32", false, "N8").named("int"),
        Some("Fval") => Inferred::known("rust_decimal::Decimal", false, "N8").named("numeric"),
        Some("Boolval") => Inferred::known("bool", false, "N8").named("bool"),
        Some("Sval") => Inferred::known("String", false, "N8").named("text"),
        Some("Bsval") => Inferred::known("Vec<u8>", false, "N8").named("bits"),
        _ => Inferred::unknown("N8"),
    }
}

fn type_name(node: &Obj) -> Option<String> {
    let tn = object(node, "type_name")?;
    svals(list(tn, "names")).next_back().map(str::to_owned)
}

/// `SQLValueFunction.op`, from pg_query's `SqlValueFunctionOp`:
/// `CURRENT_DATE` = 1, `CURRENT_TIME` = 2..3, `CURRENT_TIMESTAMP` = 4..5,
/// `LOCALTIME` = 6..7, `LOCALTIMESTAMP` = 8..9, the user/catalog names
/// = 10..15. Rule N8: every one of them is always defined.
fn sql_value_function(op: i64) -> Inferred {
    match op {
        1 => Inferred::known("chrono::NaiveDate", false, "N8").named("current_date"),
        2 | 3 | 6 | 7 => Inferred::known("chrono::NaiveTime", false, "N8").named("current_time"),
        4 | 5 => {
            Inferred::known("chrono::DateTime<chrono::Utc>", false, "N8").named("current_timestamp")
        }
        8 | 9 => Inferred::known("chrono::NaiveDateTime", false, "N8").named("localtimestamp"),
        10..=15 => Inferred::known("String", false, "N8").named("current_user"),
        _ => Inferred::unknown("N8"),
    }
}

/// PostgreSQL's own widening: `sum(int2|int4)` is `bigint`, `sum(int8)` and
/// `sum(numeric)` are `numeric`, `sum(float)` stays floating.
fn widen_sum(arg: Option<&str>) -> String {
    match arg {
        Some("i16") | Some("i32") => "i64",
        Some("i64") => "rust_decimal::Decimal",
        Some("f32") => "f32",
        Some("f64") => "f64",
        Some("rust_decimal::Decimal") => "rust_decimal::Decimal",
        _ => "rust_decimal::Decimal",
    }
    .to_owned()
}

/// `avg` is `numeric` for every integer input and `double precision` for
/// floating ones.
fn widen_avg(arg: Option<&str>) -> String {
    match arg {
        Some("f32") | Some("f64") => "f64",
        _ => "rust_decimal::Decimal",
    }
    .to_owned()
}
