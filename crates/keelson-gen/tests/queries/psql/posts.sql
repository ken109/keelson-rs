-- Hand-written PostgreSQL against tests/schema/psql.sql.
--
-- The expected output types, including nullability, are written out by hand
-- in tests/queries_psql.rs from the DDL and the SQL below; the generated code
-- is then checked against them, and (under --features live-docker) against
-- what the server actually returns.

-- name: posts_for_user :many
-- Posts by one user, newest first.
--
-- The plain case: every column's nullability is the DDL's (rule N1), and both
-- placeholders take their type from where they sit — `$1` from the column it
-- is compared with (P1), `$2` from being a row count (P3).
SELECT p.id, p.title, p.status, p.views, p.published_at
FROM posts p
WHERE p.user_id = $1
ORDER BY p.published_at DESC
LIMIT $2;

-- name: comments_with_author :many
-- Comments on one post, with the author when there is one.
--
-- `users` is LEFT-joined through a nullable foreign key, so every one of its
-- columns comes back nullable however the DDL declares it (rule N2) — and
-- because the *whole* side is in that position, it becomes one
-- `Option<…Author>` whose fields go back to their own nullability.
SELECT c.id,
       c.body,
       u.id    AS author__id,
       u.name  AS author__name,
       u.email AS author__email
FROM comments c
LEFT JOIN users u ON u.id = c.user_id
WHERE c.post_id = $1
ORDER BY c.id;

-- name: user_stats :many
-- One row per user with their post counts.
--
-- The aggregate rules: `count` is never NULL even over an empty group (N4),
-- every other aggregate is (N5), and `coalesce` is NULL only when all of its
-- arguments are (N7). `u.email` is filtered with `IS NOT NULL` and stays
-- `Option<String>` — a filter narrows the rows, not the type (N3).
SELECT u.id,
       u.name,
       u.email,
       count(p.id)               AS post_count,
       max(p.views)              AS best_views,
       coalesce(sum(p.views), 0) AS total_views
FROM users u
LEFT JOIN posts p ON p.user_id = u.id
WHERE u.email IS NOT NULL
GROUP BY u.id, u.name, u.email
ORDER BY u.id;

-- name: post_flags :many
-- Derived booleans and a cast, one per expression rule.
--
-- `IS NOT NULL` is a plain boolean (N11); an operator propagates NULL from any
-- operand, so `p.status = 'published'` is nullable while `p.views > $1` is not
-- (N10); a `CASE` with an `ELSE` is not null and one without is (N9); a cast
-- fixes the type and keeps the operand's nullability (N13).
SELECT p.id,
       p.status IS NOT NULL                          AS has_status,
       p.views > $1                                  AS is_popular,
       p.status = 'published'                        AS is_published,
       CASE WHEN p.views > 100 THEN 'hot' ELSE 'cold' END AS heat,
       CASE WHEN p.views > 100 THEN 'hot' END        AS maybe_heat,
       p.views::bigint                               AS views_wide
FROM posts p
ORDER BY p.id;

-- name: posts_with_tags :many
-- The to-many nested shape: `tags.name` collects into `row.tags`, folded from
-- the flat result rows on every non-nested field.
SELECT p.id, p.title, t.name AS "tags.name"
FROM posts p
LEFT JOIN post_tags pt ON pt.post_id = p.id
LEFT JOIN tags t ON t.id = pt.tag_id
ORDER BY p.id, t.name;

-- name: user_by_id :one
-- Exactly one row, and no annotation needed anywhere.
SELECT u.id, u.name, u.email, u.age, u.is_active, u.created_at
FROM users u
WHERE u.id = $1;

-- name: annotated :many
-- Where inference refuses, the annotations settle it: `-- column:` fixes a
-- type the generator will not guess, `-- nullable:` overrides the inferred
-- nullability (rule N16), and `-- param:` names and types a placeholder.
-- param: $1 title_pattern String
-- column: shouty String
-- nullable: shouty false
SELECT p.id, upper(p.title) AS shouty
FROM posts p
WHERE p.title LIKE $1
ORDER BY p.id;

-- name: titles_union :many
-- A set operation: the query face runs it, and the mod face is refused in
-- writing rather than faked by nesting it as a sub-select.
SELECT p.title AS title FROM posts p
UNION ALL
SELECT t.name AS title FROM tags t;

-- name: user_by_email :optional
-- Zero or one row: the `Option<Row>` cardinality.
SELECT u.id, u.name
FROM users u
WHERE u.email = $1;

-- name: bump_views :exec
-- A statement run for its side effect: no row struct at all, and no mod face
-- (an UPDATE has no clause a host SELECT could absorb).
UPDATE posts SET views = views + 1 WHERE id = $1;

-- name: comments_with_prefixed_author :many
-- `-- prefix:` switches the nested-row separator for this query alone:
-- `author_id`/`author_name` would be two flat fields under the defaults, and
-- are one nested `author` here.
-- prefix: author_
SELECT c.id, u.id AS author_id, u.name AS author_name
FROM comments c
LEFT JOIN users u ON u.id = c.user_id
ORDER BY c.id;
