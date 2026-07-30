CREATE TABLE users (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    email      TEXT,
    age        INTEGER,
    is_active  INTEGER NOT NULL DEFAULT 1,
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

-- Two views — see the note in tests/schema/psql.sql. SQLite writes through
-- neither: a view is read-only there unless it carries INSTEAD OF triggers,
-- and these carry none.
CREATE VIEW user_emails AS
    SELECT id, email FROM users;

CREATE VIEW post_authors AS
    SELECT p.id AS post_id, p.title AS title, u.id AS user_id, u.name AS user_name
    FROM posts p JOIN users u ON u.id = p.user_id;
