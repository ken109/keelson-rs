-- The shared test schema (tests/schema/sqlite.sql) in its *executable*
-- rendition — the same one the spec model's live DDL uses: `is_active` is
-- declared BOOLEAN (the grammar copy spells it INTEGER only because grammar
-- tests never execute), everything else is verbatim. Plus a third view the
-- shared schema does not carry: SQLite writes through a view only when it has
-- INSTEAD OF triggers for all three statements, and `editable_users` is what
-- proves the generator reads that rule the same way the engine does.
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

-- A cycle of two to-one relations, which is what forces every generated
-- to-one `rel` field to be boxed: a thread names the message that opened it
-- and a message names its thread, so `Thread.rel.first_message` and
-- `Message.rel.thread` refer to each other. Unboxed, that is a recursive type
-- of infinite size and the generated code does not compile.
--
-- SQLite resolves a foreign key's target lazily — the constraint is checked at
-- DML time, and only when the foreign_keys pragma is on — so the forward
-- reference to `messages` in the first statement is legal, and no deferred
-- constraint or ALTER TABLE is needed. PostgreSQL and MySQL both resolve the
-- target at DDL time and need one of those, which is why their renditions of
-- this pair in tests/schema/ are spelled differently.
--
-- Comments in this file carry neither a statement terminator nor either of the
-- keywords that bracket a trigger body: one test splits the file on exactly
-- those before handing the statements to a pool one at a time.
CREATE TABLE threads (
    id               INTEGER PRIMARY KEY,
    title            TEXT NOT NULL,
    first_message_id INTEGER REFERENCES messages (id)
);

CREATE TABLE messages (
    id        INTEGER PRIMARY KEY,
    thread_id INTEGER NOT NULL REFERENCES threads (id),
    body      TEXT NOT NULL
);

CREATE VIEW user_emails AS
    SELECT id, email FROM users;

CREATE VIEW post_authors AS
    SELECT p.id AS post_id, p.title AS title, u.id AS user_id, u.name AS user_name
    FROM posts p JOIN users u ON u.id = p.user_id;

-- The one view SQLite will write through: all three INSTEAD OF triggers, so
-- `pg_relation_is_updatable`'s SQLite equivalent (reading sqlite_master) says
-- yes and `[tables.editable_users] key` is accepted.
CREATE VIEW editable_users AS
    SELECT id, name, email FROM users;

CREATE TRIGGER editable_users_insert INSTEAD OF INSERT ON editable_users
BEGIN
    INSERT INTO users (id, name, email) VALUES (NEW.id, NEW.name, NEW.email);
END;

CREATE TRIGGER editable_users_update INSTEAD OF UPDATE ON editable_users
BEGIN
    UPDATE users SET name = NEW.name, email = NEW.email WHERE id = OLD.id;
END;

CREATE TRIGGER editable_users_delete INSTEAD OF DELETE ON editable_users
BEGIN
    DELETE FROM users WHERE id = OLD.id;
END;
