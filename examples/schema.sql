-- The schema every example works against: a small blog, in SQLite.
--
-- keelson does not own this file. It is the shape your migration tool
-- (`sqlx migrate`, Atlas, refinery, …) left in the database, and it is what
-- `keelson-gen` introspects to write `src/models/`. The loop is
-- migrate -> regenerate -> compile.
--
-- Deliberately varied, so the generated models cover more than one shape:
--
--   users        a plain table, the root of every relation here
--   posts        a NOT NULL foreign key -- a required parent
--   comments     a NULLABLE foreign key -- an optional parent
--   tags         a UNIQUE column, which the factory generator turns into a
--                sequence-backed value source
--   post_tags    a composite primary key, i.e. a join table
--   audit_logs   written only by an after-insert hook
--   post_authors a VIEW, which has no key and no foreign keys of its own --
--                so its relations are declared in keelson.toml
--
-- No statement below spans a semicolon inside a string or a trigger body, and
-- neither does any comment: Sandbox splits this file on the semicolon before
-- handing the statements to the pool one at a time.

CREATE TABLE users (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    email      TEXT,
    age        INTEGER,
    is_active  BOOLEAN NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE posts (
    id           INTEGER PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users (id),
    title        TEXT NOT NULL,
    status       TEXT,
    views        INTEGER NOT NULL DEFAULT 0,
    published_at TEXT
);

CREATE TABLE comments (
    id         INTEGER PRIMARY KEY,
    post_id    INTEGER NOT NULL REFERENCES posts (id),
    user_id    INTEGER REFERENCES users (id),
    body       TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE tags (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE post_tags (
    post_id INTEGER NOT NULL REFERENCES posts (id),
    tag_id  INTEGER NOT NULL REFERENCES tags (id),
    PRIMARY KEY (post_id, tag_id)
);

CREATE TABLE audit_logs (
    id        INTEGER PRIMARY KEY,
    entity    TEXT NOT NULL,
    entity_id INTEGER NOT NULL,
    note      TEXT NOT NULL
);

CREATE VIEW post_authors AS
    SELECT p.id AS post_id, p.title AS title, u.id AS user_id, u.name AS user_name
    FROM posts p JOIN users u ON u.id = p.user_id;
