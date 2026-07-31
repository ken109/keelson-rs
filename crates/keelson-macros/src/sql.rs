//! The `sql!` scanner: a SQL string literal with `{…}` holes becomes a
//! constructor call plus one `bind` per hole.
//!
//! # What the holes are for
//!
//! `RawQuery::bind` is positional, and the template scan that rewrites `?`
//! does not track quoting. Two error classes follow, and neither is caught by
//! anything:
//!
//! - **A transposed pair of binds.** Five `?` and five `.bind(…)` calls in the
//!   wrong order type-check and run; only the answers are wrong.
//! - **A `?` inside a string literal.** `WHERE note = 'what?'` has a hole in
//!   it as far as the scan is concerned. When the argument count happens to
//!   match, the statement is silently corrupt — the hazard is documented in
//!   `docs/sql-rendering.md` and is exactly the sort of thing a macro can make
//!   unrepresentable.
//!
//! Writing the value at the hole removes the first by construction. Escaping
//! every `?` the author typed removes the second: after this scan the only
//! holes in the emitted string are the ones the macro put there.
//!
//! # The grammar
//!
//! ```text
//! {expr}       bind the value of `expr`     -> .bind(expr)
//! {expr:sql}   splice `expr` as SQL         -> .bind_expr(expr)
//! {{  }}       a literal brace
//! ```
//!
//! `expr` is any Rust expression, so `{user.id}` and `{limit + 1}` work; the
//! common case is a bare variable name, as in `format!`.
//!
//! # The limitation, recorded
//!
//! Spans inside a string literal are not addressable on stable
//! (`Literal::subspan` is unstable), so a type error in a hole points at the
//! macro call rather than at the hole. Reopen when `subspan` stabilises: the
//! scanner already tracks byte offsets, which is what it would need.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Error, Expr, LitStr, Path, Result, Token};

/// `sql_with!(path::to::constructor, "…")` — what the dialect crates' own
/// `sql!` forwards to, with their `raw_query` as the path.
pub(crate) struct Input {
    constructor: Path,
    sql: LitStr,
}

impl Parse for Input {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let constructor = input.parse()?;
        input.parse::<Token![,]>()?;
        let sql = input.parse()?;
        // A trailing comma is the only thing left that is allowed.
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        }
        if !input.is_empty() {
            return Err(input.error(
                "`sql!` takes one string literal. Values go in `{…}` holes inside it, not as \
                 further arguments",
            ));
        }
        Ok(Input { constructor, sql })
    }
}

/// One `{…}` hole.
enum Hole {
    /// `{expr}` — bind the value.
    Value(Expr),
    /// `{expr:sql}` — splice the expression.
    Sql(Expr),
}

pub(crate) fn expand(input: Input) -> Result<TokenStream> {
    let Input { constructor, sql } = input;
    let (text, holes) = scan(&sql.value(), sql.span())?;

    let binds = holes.into_iter().map(|hole| match hole {
        Hole::Value(e) => quote!(.bind(#e)),
        Hole::Sql(e) => quote!(.bind_expr(#e)),
    });

    Ok(quote! {
        #constructor(#text) #(#binds)*
    })
}

/// Rewrite the literal into template text, collecting the holes.
///
/// Two rules, applied in one left-to-right pass:
///
/// - `{…}` becomes a `?` and yields a hole; `{{` and `}}` are literal braces.
/// - **every other `?` is escaped to `\?`**, so a question mark the author
///   typed — in a string literal, in PostgreSQL's `?|` operator — stays a
///   question mark instead of becoming a hole downstream.
fn scan(sql: &str, span: Span) -> Result<(String, Vec<Hole>)> {
    let mut text = String::with_capacity(sql.len());
    let mut holes = Vec::new();
    let mut rest = sql;

    while let Some(i) = rest.find(['{', '}', '?']) {
        text.push_str(&rest[..i]);
        let (matched, tail) = rest.split_at(i);
        let _ = matched;
        rest = tail;

        match rest.as_bytes()[0] {
            b'?' => {
                // Not a hole: the author wrote it, so it survives as text.
                text.push_str("\\?");
                rest = &rest[1..];
            }
            b'}' => {
                if let Some(tail) = rest.strip_prefix("}}") {
                    text.push('}');
                    rest = tail;
                } else {
                    return Err(Error::new(
                        span,
                        "unmatched `}` in the SQL. Write `}}` for a literal closing brace",
                    ));
                }
            }
            _ => {
                if let Some(tail) = rest.strip_prefix("{{") {
                    text.push('{');
                    rest = tail;
                    continue;
                }
                let end = rest.find('}').ok_or_else(|| {
                    Error::new(
                        span,
                        "unclosed `{` in the SQL. Write `{{` for a literal opening brace",
                    )
                })?;
                holes.push(parse_hole(&rest[1..end], span)?);
                text.push('?');
                rest = &rest[end + 1..];
            }
        }
    }
    text.push_str(rest);
    Ok((text, holes))
}

/// `expr` or `expr:sql`.
fn parse_hole(body: &str, span: Span) -> Result<Hole> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(Error::new(
            span,
            "an empty `{}` has nothing to bind. Name the value: `{user_id}`",
        ));
    }
    // Only a `:sql` suffix is a spec; anything else belongs to the expression
    // (`a::b`, a struct literal's `field: value`), so the split is on the
    // suffix rather than on the first colon.
    let (source, hole): (&str, fn(Expr) -> Hole) = match trimmed.strip_suffix(":sql") {
        Some(head) => (head, Hole::Sql),
        None => (trimmed, Hole::Value),
    };
    // Parsed *through a `LitStr` carrying the original literal's span*, not
    // with `parse_str`. The span is what fixes hygiene: a name the caller
    // wrote has to resolve at the caller's site, and tokens synthesised at the
    // proc macro's own call site would resolve inside the forwarding
    // `macro_rules!` instead — where the caller's locals do not exist.
    let expr: Expr = LitStr::new(source.trim(), span).parse().map_err(|e| {
        Error::new(
            span,
            format!("`{{{trimmed}}}` is not a Rust expression: {e}"),
        )
    })?;
    Ok(hole(expr))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(sql: &str) -> (String, usize) {
        let (text, holes) = scan(sql, Span::call_site()).expect("scan");
        (text, holes.len())
    }

    #[test]
    fn a_hole_becomes_a_placeholder() {
        assert_eq!(
            text_of("SELECT * FROM t WHERE a = {x} AND b > {y}"),
            ("SELECT * FROM t WHERE a = ? AND b > ?".to_owned(), 2)
        );
    }

    /// The whole point: a question mark the author wrote is text, not a hole.
    #[test]
    fn an_authors_question_mark_is_escaped_rather_than_captured() {
        assert_eq!(
            text_of("SELECT * FROM t WHERE note = 'what?' AND a = {x}"),
            (
                r"SELECT * FROM t WHERE note = 'what\?' AND a = ?".to_owned(),
                1
            )
        );
    }

    #[test]
    fn doubled_braces_are_literal() {
        assert_eq!(
            text_of(r#"SELECT '{{"a": 1}}'::jsonb"#),
            (r#"SELECT '{"a": 1}'::jsonb"#.to_owned(), 0)
        );
    }

    #[test]
    fn a_spliced_hole_is_still_one_placeholder() {
        let (text, holes) =
            scan("SELECT * FROM t WHERE id IN ({ids:sql})", Span::call_site()).expect("scan");
        assert_eq!(text, "SELECT * FROM t WHERE id IN (?)");
        assert!(matches!(holes[0], Hole::Sql(_)));
    }

    #[test]
    fn a_path_in_a_hole_is_not_mistaken_for_a_spec() {
        let (_, holes) = scan("SELECT {Config::LIMIT}", Span::call_site()).expect("scan");
        assert!(matches!(holes[0], Hole::Value(_)));
    }

    #[test]
    fn unbalanced_braces_are_refused_by_name() {
        for (sql, wanted) in [
            ("SELECT {x", "unclosed"),
            ("SELECT x}", "unmatched"),
            ("SELECT {}", "nothing to bind"),
        ] {
            // Matched rather than `unwrap_err`ed: the success type holds
            // `syn::Expr`, which has no `Debug` unless syn's `extra-traits` is
            // on, and this crate deliberately does not ask for it.
            match scan(sql, Span::call_site()) {
                Ok(_) => panic!("{sql} should not scan"),
                Err(e) => assert!(e.to_string().contains(wanted), "{sql}: {e}"),
            }
        }
    }
}
