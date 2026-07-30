-- The shared test schema (tests/schema/sqlite.sql) in its *executable*
-- rendition — the same one the spec model's live DDL uses: `is_active` is
-- declared BOOLEAN (the grammar copy spells it INTEGER only because grammar
-- tests never execute), everything else is verbatim. Plus one view, so the
-- SELECT-only surface is generated too.
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

CREATE VIEW user_emails AS SELECT id, email FROM users;
