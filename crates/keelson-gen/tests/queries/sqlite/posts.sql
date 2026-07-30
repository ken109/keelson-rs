-- Hand-written SQLite against tests/fixtures/sqlite_schema.sql — the same
-- queries as tests/queries/psql/posts.sql, spelled for SQLite: `?n`
-- placeholders, and the types SQLite's weaker schema can actually justify
-- (every integer column is `i64`, a comparison yields an integer rather than a
-- boolean, and `sum` does not widen).

-- name: posts_for_user :many
-- Posts by one user, newest first.
SELECT p.id, p.title, p.status, p.views, p.published_at
FROM posts p
WHERE p.user_id = ?1
ORDER BY p.published_at DESC
LIMIT ?2;

-- name: comments_with_author :many
-- Comments on one post, with the author when there is one — rule N2 through a
-- nullable foreign key, so the whole `author` side is one `Option`.
SELECT c.id,
       c.body,
       u.id    AS author__id,
       u.name  AS author__name,
       u.email AS author__email
FROM comments c
LEFT JOIN users u ON u.id = c.user_id
WHERE c.post_id = ?1
ORDER BY c.id;

-- name: user_stats :many
-- The aggregate rules: N4 for `count`, N5 for the rest, N7 for `coalesce`,
-- and N3 — an `IS NOT NULL` filter leaves `email` an `Option<String>`.
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
-- The expression rules, SQLite-flavoured: comparisons are integers, not
-- booleans.
SELECT p.id,
       p.status IS NOT NULL   AS has_status,
       p.views > ?1           AS is_popular,
       p.status = 'published' AS is_published,
       CASE WHEN p.views > 100 THEN 'hot' ELSE 'cold' END AS heat,
       CASE WHEN p.views > 100 THEN 'hot' END             AS maybe_heat,
       CAST(p.views AS TEXT)  AS views_text
FROM posts p
ORDER BY p.id;

-- name: posts_with_tags :many
-- The to-many nested shape.
SELECT p.id, p.title, t.name AS "tags.name"
FROM posts p
LEFT JOIN post_tags pt ON pt.post_id = p.id
LEFT JOIN tags t ON t.id = pt.tag_id
ORDER BY p.id, t.name;

-- name: user_by_id :one
-- Exactly one row.
SELECT u.id, u.name, u.email, u.age, u.is_active, u.created_at
FROM users u
WHERE u.id = ?1;

-- name: annotated :many
-- The annotations settle what inference will not guess.
-- param: ?1 title_pattern String
-- column: shouty String
-- nullable: shouty false
SELECT p.id, upper(p.title) AS shouty
FROM posts p
WHERE p.title LIKE ?1
ORDER BY p.id;

-- name: titles_union :many
-- A compound select: the query face runs it (and rule N14 merges the arms'
-- nullability), while the mod face is refused in writing.
SELECT p.title AS title FROM posts p
UNION ALL
SELECT t.name AS title FROM tags t;

-- name: user_by_email :optional
-- Zero or one row.
SELECT u.id, u.name
FROM users u
WHERE u.email = ?1;

-- name: bump_views :exec
-- Run for its side effect: no row struct, and no mod face.
UPDATE posts SET views = views + 1 WHERE id = ?1;

-- name: hot_or_recent :many
-- A top-level `OR` in the WHERE: as a mod it merges into the host's `AND`
-- chain, so the fragment has to arrive parenthesised or it re-binds.
SELECT p.id, p.title
FROM posts p
WHERE p.views > ?1 OR p.status = 'published';
