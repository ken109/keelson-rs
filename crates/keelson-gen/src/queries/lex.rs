//! Tokens with byte offsets, and the clause map built from them.
//!
//! The mod face needs to hand the host statement *the author's own bytes*, one
//! clause at a time, so the analysis has to know where each clause starts and
//! ends. Parse trees are the wrong tool for that: `pg_query`'s nodes carry a
//! start `location` and no end, and `sqlite3-parser` carries neither. A token
//! stream carries both, exactly.
//!
//! So each dialect's front end produces the same flat [`Token`] list —
//! PostgreSQL through `pg_query::scan`, which is the server's own scanner;
//! SQLite through the small scanner below — and [`clauses`] turns either into
//! the same [`Clauses`]. The clause logic is written once and tested once.

use crate::queries::ir::{Clauses, Span};

/// What a token is, as far as clause-splitting cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A bare word — a keyword or an identifier — upper-cased.
    Word(String),
    /// `(`
    Open,
    /// `)`
    Close,
    /// A placeholder, with its 1-based number.
    Placeholder(usize),
    /// Anything else: operators, literals, punctuation, comments.
    Other,
}

/// One token and where it sits in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The byte span, in the *file's* coordinates.
    pub span: Span,
    /// What it is.
    pub kind: Kind,
}

impl Token {
    fn word(&self) -> Option<&str> {
        match &self.kind {
            Kind::Word(w) => Some(w),
            _ => None,
        }
    }
}

/// Classify one token's text. Shared by both front ends so `$1` is a
/// placeholder however the scanner spelled it.
fn classify(text: &str) -> Kind {
    match text {
        "(" => return Kind::Open,
        ")" => return Kind::Close,
        _ => {}
    }
    let mut chars = text.chars();
    match chars.next() {
        Some('$') | Some('?') => {
            let rest = &text[1..];
            if !rest.is_empty()
                && rest.bytes().all(|b| b.is_ascii_digit())
                && let Ok(n) = rest.parse::<usize>()
            {
                return Kind::Placeholder(n);
            }
            Kind::Other
        }
        Some(c) if c.is_alphabetic() || c == '_' => {
            if text.chars().all(|c| c.is_alphanumeric() || c == '_') {
                Kind::Word(text.to_ascii_uppercase())
            } else {
                Kind::Other
            }
        }
        _ => Kind::Other,
    }
}

/// PostgreSQL: tokens straight from libpg_query's scanner.
///
/// `offset` is where `sql` sits in the file, so every span comes back in file
/// coordinates.
pub fn scan_psql(sql: &str, offset: usize) -> Result<Vec<Token>, String> {
    let scanned = pg_query::scan(sql).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(scanned.tokens.len());
    for t in &scanned.tokens {
        let (start, end) = (t.start.max(0) as usize, t.end.max(0) as usize);
        if start >= end || end > sql.len() {
            continue;
        }
        out.push(Token {
            span: Span {
                start: start + offset,
                end: end + offset,
            },
            kind: classify(&sql[start..end]),
        });
    }
    Ok(out)
}

/// SQLite: a scanner of exactly the lexical forms SQLite has —
/// `'…'` (`''` escapes), `"…"`/`[…]`/`` `…` `` identifiers, `--` line and
/// `/* … */` block comments, `?`/`?N`/`:name`/`@name`/`$name` parameters,
/// `X'…'` blobs, and everything else one character at a time.
pub fn scan_sqlite(sql: &str, offset: usize) -> Result<Vec<Token>, String> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let push = |out: &mut Vec<Token>, s: usize, e: usize, kind: Kind| {
        out.push(Token {
            span: Span {
                start: s + offset,
                end: e + offset,
            },
            kind,
        });
    };
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Comments.
        if c == b'-' && i + 1 < b.len() && b[i + 1] == b'-' {
            let start = i;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            push(&mut out, start, i, Kind::Other);
            continue;
        }
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            push(&mut out, start, i, Kind::Other);
            continue;
        }
        // Quoted forms.
        if c == b'\'' || c == b'"' || c == b'`' {
            let start = i;
            i += 1;
            loop {
                if i >= b.len() {
                    return Err(format!("unterminated {} literal", c as char));
                }
                if b[i] == c {
                    if i + 1 < b.len() && b[i + 1] == c {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            // A double-quoted identifier is still an identifier, but never a
            // clause keyword, so `Other` is the honest classification.
            push(&mut out, start, i, Kind::Other);
            continue;
        }
        if c == b'[' {
            let start = i;
            while i < b.len() && b[i] != b']' {
                i += 1;
            }
            i = (i + 1).min(b.len());
            push(&mut out, start, i, Kind::Other);
            continue;
        }
        if c == b'(' {
            push(&mut out, i, i + 1, Kind::Open);
            i += 1;
            continue;
        }
        if c == b')' {
            push(&mut out, i, i + 1, Kind::Close);
            i += 1;
            continue;
        }
        // Parameters: `?`, `?12`, `:name`, `@name`, `$name`.
        if c == b'?' || c == b':' || c == b'@' || c == b'$' {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            push(&mut out, start, i, classify(&sql[start..i]));
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            let start = i;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] >= 0x80)
            {
                i += 1;
            }
            push(&mut out, start, i, classify(&sql[start..i]));
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                i += 1;
            }
            push(&mut out, start, i, Kind::Other);
            continue;
        }
        // Operators and punctuation: one character each is enough, because
        // nothing downstream looks at them.
        push(&mut out, i, i + 1, Kind::Other);
        i += 1;
    }
    Ok(out)
}

/// Every placeholder occurrence, in text order.
pub fn placeholders(tokens: &[Token]) -> Vec<crate::queries::ir::Placeholder> {
    tokens
        .iter()
        .filter_map(|t| match t.kind {
            Kind::Placeholder(n) => Some(crate::queries::ir::Placeholder {
                span: t.span,
                number: n,
            }),
            _ => None,
        })
        .collect()
}

/// The keywords that open a clause at depth 0.
const CLAUSE_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP", "HAVING", "WINDOW", "ORDER", "LIMIT", "OFFSET", "FETCH",
    "FOR",
];

/// The keywords that mean this statement has no mod face at all.
const REFUSALS: &[(&str, &str)] = &[
    (
        "UNION",
        "a set operation has no single WHERE/FROM to merge into a host statement",
    ),
    (
        "INTERSECT",
        "a set operation has no single WHERE/FROM to merge into a host statement",
    ),
    (
        "EXCEPT",
        "a set operation has no single WHERE/FROM to merge into a host statement",
    ),
    (
        "WITH",
        "a CTE would have to be hoisted into the host statement's WITH clause, which \
         would change name resolution in the host",
    ),
];

/// Split a token stream into clause bodies.
///
/// `end` bounds the statement (the `-- name:` span), so trailing text in the
/// file never leaks into the last clause.
pub fn clauses(tokens: &[Token], start: usize, end: usize) -> Clauses {
    let mut c = Clauses::default();
    let inside: Vec<&Token> = tokens
        .iter()
        .filter(|t| t.span.start >= start && t.span.end <= end)
        .collect();

    let first = inside.iter().find(|t| !matches!(t.kind, Kind::Other));
    match first.and_then(|t| t.word()) {
        Some("SELECT") => {}
        // A leading `WITH` is a refusal in its own right, and its reason is the
        // useful one — say that rather than "not a SELECT".
        Some(w) if REFUSALS.iter().any(|(k, _)| *k == w) => {
            c.unsupported = REFUSALS
                .iter()
                .find(|(k, _)| *k == w)
                .map(|(_, why)| (*why).to_owned());
            return c;
        }
        _ => {
            c.unsupported = Some(
                "only a SELECT has a mod face: an INSERT/UPDATE/DELETE has no clause a \
                 host SELECT could absorb"
                    .to_owned(),
            );
            return c;
        }
    }

    // Depth-0 clause keyword positions: (keyword, keyword token index).
    let mut depth = 0i32;
    let mut marks: Vec<(&str, usize)> = Vec::new();
    for (i, t) in inside.iter().enumerate() {
        match t.kind {
            Kind::Open => depth += 1,
            Kind::Close => depth -= 1,
            _ => {}
        }
        if depth != 0 {
            continue;
        }
        let Some(word) = t.word() else { continue };
        if let Some((_, why)) = REFUSALS.iter().find(|(k, _)| *k == word) {
            c.unsupported = Some((*why).to_owned());
            return c;
        }
        if CLAUSE_KEYWORDS.contains(&word) {
            marks.push((word, i));
        }
    }

    for (n, (word, i)) in marks.iter().enumerate() {
        // The clause body runs from just after the keyword (two words for
        // `GROUP BY`/`ORDER BY`) to the next depth-0 clause keyword.
        let mut body_start_tok = i + 1;
        if matches!(*word, "GROUP" | "ORDER")
            && inside.get(body_start_tok).and_then(|t| t.word()) == Some("BY")
        {
            body_start_tok += 1;
        }
        let body_end_tok = marks.get(n + 1).map_or(inside.len(), |(_, j)| *j);
        if body_start_tok >= body_end_tok {
            continue;
        }
        let span = Span {
            start: inside[body_start_tok].span.start,
            end: inside[body_end_tok - 1].span.end,
        };
        let slot = match *word {
            "SELECT" => &mut c.select_list,
            "FROM" => &mut c.from,
            "WHERE" => &mut c.where_,
            "GROUP" => &mut c.group_by,
            "HAVING" => &mut c.having,
            "ORDER" => &mut c.order_by,
            "LIMIT" => &mut c.limit,
            "OFFSET" => &mut c.offset,
            // FETCH and FOR UPDATE have no keelson clause a host SELECT
            // merges them into without changing meaning; a query carrying one
            // keeps its query face and loses its mod face.
            _ => {
                c.unsupported = Some(format!(
                    "the `{word}` clause has no host-statement counterpart to merge into"
                ));
                return c;
            }
        };
        *slot = Some(span);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(sql: &str) -> (Clauses, String) {
        let tokens = scan_psql(sql, 0).expect("scan");
        (clauses(&tokens, 0, sql.len()), sql.to_owned())
    }

    #[track_caller]
    fn body(c: &Option<Span>, sql: &str) -> String {
        c.map(|s| s.of(sql).to_owned()).unwrap_or_default()
    }

    #[test]
    fn every_clause_body_is_the_authors_own_bytes() {
        let sql = "SELECT a, b FROM t JOIN u ON u.id = t.uid WHERE a > $1 \
                   GROUP BY a HAVING count(*) > 2 ORDER BY b DESC LIMIT 10 OFFSET 5";
        let (c, s) = spans(sql);
        assert_eq!(body(&c.select_list, &s), "a, b");
        assert_eq!(body(&c.from, &s), "t JOIN u ON u.id = t.uid");
        assert_eq!(body(&c.where_, &s), "a > $1");
        assert_eq!(body(&c.group_by, &s), "a");
        assert_eq!(body(&c.having, &s), "count(*) > 2");
        assert_eq!(body(&c.order_by, &s), "b DESC");
        assert_eq!(body(&c.limit, &s), "10");
        assert_eq!(body(&c.offset, &s), "5");
        assert!(c.unsupported.is_none());
    }

    #[test]
    fn a_keyword_inside_parentheses_is_not_a_clause_boundary() {
        let sql = "SELECT a FROM t WHERE id IN (SELECT id FROM u WHERE u.x = 1) AND b = 2";
        let (c, s) = spans(sql);
        assert_eq!(body(&c.from, &s), "t");
        assert_eq!(
            body(&c.where_, &s),
            "id IN (SELECT id FROM u WHERE u.x = 1) AND b = 2"
        );
    }

    #[test]
    fn a_keyword_inside_a_string_literal_is_not_a_clause_boundary() {
        let sql = "SELECT a FROM t WHERE name = 'from where limit'";
        let (c, s) = spans(sql);
        assert_eq!(body(&c.from, &s), "t");
        assert_eq!(body(&c.where_, &s), "name = 'from where limit'");
    }

    #[test]
    fn set_operations_and_ctes_refuse_the_mod_face_with_a_reason() {
        let (c, _) = spans("SELECT a FROM t UNION SELECT a FROM u");
        assert!(c.unsupported.as_deref().unwrap().contains("set operation"));
        let (c, _) = spans("WITH x AS (SELECT 1) SELECT * FROM x");
        assert!(c.unsupported.as_deref().unwrap().contains("CTE"));
    }

    #[test]
    fn a_non_select_has_no_mod_face() {
        let (c, _) = spans("INSERT INTO t (a) VALUES ($1)");
        assert!(c.unsupported.as_deref().unwrap().contains("only a SELECT"));
    }

    #[test]
    fn the_sqlite_scanner_agrees_with_libpg_query_on_shared_syntax() {
        let sql = "SELECT a, b FROM t WHERE name = 'a -- b' AND x = ?1 ORDER BY b LIMIT 3";
        let pg = clauses(&scan_psql(sql, 0).unwrap(), 0, sql.len());
        let lite = clauses(&scan_sqlite(sql, 0).unwrap(), 0, sql.len());
        assert_eq!(pg, lite);
        assert_eq!(body(&lite.where_, sql), "name = 'a -- b' AND x = ?1");
    }

    #[test]
    fn placeholders_are_found_with_their_spans() {
        let sql = "SELECT a FROM t WHERE a = $1 AND b < $2 AND c = $1";
        let ph = placeholders(&scan_psql(sql, 0).unwrap());
        assert_eq!(
            ph.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![1, 2, 1]
        );
        assert_eq!(ph[0].span.of(sql), "$1");
        let ph = placeholders(&scan_sqlite(&sql.replace('$', "?"), 0).unwrap());
        assert_eq!(
            ph.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![1, 2, 1]
        );
    }

    #[test]
    fn spans_are_offset_into_the_file() {
        let file = "-- name: q :many\nSELECT a FROM t";
        let start = file.find("SELECT").unwrap();
        let tokens = scan_psql(&file[start..], start).unwrap();
        let c = clauses(&tokens, start, file.len());
        assert_eq!(body(&c.from, file), "t");
    }
}
