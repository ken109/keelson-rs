-- Layer 4: hand-written SQL, compiled into typed Rust by keelson-gen.
--
-- This file is the source of truth. `src/queries/blog.rs` is generated from
-- it and `include_str!`s it back, slicing the very bytes below to build both
-- of a query's two faces -- so the generated code and the SQL cannot disagree,
-- and editing this file without regenerating fails a `const` assertion on the
-- file's length.
--
-- Each query is introduced by `-- name: <fn> :<kind>`:
--
--   :many      -> Vec<Row>
--   :one       -> Row          (exactly one; zero rows is an error)
--   :optional  -> Option<Row>
--   :exec      -> ExecResult   (no row struct, and no mod face)
--
-- Nullability is inferred from the parse tree plus the introspected schema,
-- and every decision is written into the generated file as the rule that made
-- it (N1, N2, …). Where inference cannot know, annotate.

-- name: posts_for_user :many
-- Posts by one user, newest first.
--
-- The plain case: every column's nullability is the DDL's (rule N1), and both
-- placeholders take their type from where they sit -- `?1` from the column it
-- is compared with, `?2` from being a row count.
SELECT p.id, p.title, p.status, p.views, p.published_at
FROM posts p
WHERE p.user_id = ?1
ORDER BY p.published_at DESC
LIMIT ?2;

-- name: comments_with_author :many
-- Comments on one post, with the author when there is one.
--
-- `users` is LEFT-joined through a nullable foreign key, so every one of its
-- columns comes back nullable however the DDL declares it (rule N2) -- and
-- because the *whole* side sits in that position, the generator folds it into
-- one `Option<CommentsWithAuthorAuthor>` whose fields go back to their own
-- nullability. The `author__` prefix is what names that nested struct.
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
-- One row per user with their post counts.
--
-- The aggregate rules: `count` is never NULL even over an empty group (N4),
-- every other aggregate is (N5), and `coalesce` is NULL only when all of its
-- arguments are (N7). `u.email` is filtered with `IS NOT NULL` and still comes
-- back `Option<String>` -- a filter narrows the rows, not the type (N3).
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

-- name: posts_with_tags :many
-- The to-many nested shape: a dotted alias collects the repeated side into a
-- `Vec` of a nested struct instead of repeating the parent columns.
SELECT p.id, p.title, t.name AS "tags.name"
FROM posts p
LEFT JOIN post_tags pt ON pt.post_id = p.id
LEFT JOIN tags t ON t.id = pt.tag_id
ORDER BY p.id, t.name;

-- name: shouty_titles :many
-- Annotations settle what inference will not guess: `upper()` is not in the
-- generator's function table, so its type and nullability are stated here.
-- param: ?1 title_pattern String
-- column: shouty String
-- nullable: shouty false
SELECT p.id, upper(p.title) AS shouty
FROM posts p
WHERE p.title LIKE ?1
ORDER BY p.id;

-- name: user_by_id :one
-- Exactly one row -- zero is an error, which is the point of `:one`.
SELECT u.id, u.name, u.email, u.age, u.is_active, u.created_at
FROM users u
WHERE u.id = ?1;

-- name: user_by_email :optional
-- Zero or one row.
SELECT u.id, u.name
FROM users u
WHERE u.email = ?1;

-- name: bump_views :exec
-- Run for its side effect: no row struct, and no mod face.
UPDATE posts SET views = views + 1 WHERE id = ?1;

-- name: popular_posts :many
-- Written *without* a table alias, on purpose.
--
-- As a mod this merges into a host statement that already has its own FROM --
-- a generated model query over `posts`, say -- and the host's FROM is kept.
-- A fragment saying `p.views` would then refer to an alias the host never
-- declared, and the engine would reject the merged statement. Aliasing is
-- fine for a query that is only ever run on its own; a query meant for the
-- mod face names its columns the way the host will.
SELECT posts.id, posts.title
FROM posts
WHERE posts.views > ?1;

-- name: hot_or_recent :many
-- A top-level `OR` in the WHERE. As a mod this merges into the host's `AND`
-- chain, so the fragment has to arrive parenthesised or it would re-bind --
-- the generator emits the parentheses.
SELECT p.id, p.title
FROM posts p
WHERE p.views > ?1 OR p.status = 'published';
