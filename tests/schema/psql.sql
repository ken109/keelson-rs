CREATE TABLE users (
    id         integer PRIMARY KEY,
    name       text NOT NULL,
    email      text,
    age        integer,
    is_active  boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE posts (
    id           integer PRIMARY KEY,
    user_id      integer NOT NULL REFERENCES users (id),
    title        text NOT NULL,
    status       text,
    views        integer NOT NULL DEFAULT 0,
    published_at timestamptz
);

CREATE TABLE comments (
    id         integer PRIMARY KEY,
    post_id    integer NOT NULL REFERENCES posts (id),
    user_id    integer REFERENCES users (id),
    body       text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tags (
    id   integer PRIMARY KEY,
    name text NOT NULL UNIQUE
);

CREATE TABLE post_tags (
    post_id integer NOT NULL REFERENCES posts (id),
    tag_id  integer NOT NULL REFERENCES tags (id),
    PRIMARY KEY (post_id, tag_id)
);

-- A cycle of two to-one relations: a thread names the message that opened it
-- and a message names its thread. Layer 4 generates a `rel` field per relation
-- holding the target's whole row, so this pair is what forces every to-one
-- field to be boxed — unboxed it is a recursive type of infinite size and the
-- generated code does not compile.
--
-- PostgreSQL resolves a foreign key's target at DDL time, so the pair cannot be
-- created by two plain CREATE TABLEs whichever order they are in: the second
-- constraint is added afterwards.
CREATE TABLE threads (
    id               integer PRIMARY KEY,
    title            text NOT NULL,
    first_message_id integer
);

CREATE TABLE messages (
    id        integer PRIMARY KEY,
    thread_id integer NOT NULL REFERENCES threads (id),
    body      text NOT NULL
);

ALTER TABLE threads ADD CONSTRAINT threads_first_message_id_fkey
    FOREIGN KEY (first_message_id) REFERENCES messages (id);

-- Two views, so the SELECT-only surface has something to be generated from,
-- and so a relation that touches a view has somewhere to land. They differ in
-- what the engines will write through, which is the point: `user_emails`
-- projects one table (PostgreSQL and MySQL write through it, SQLite does not),
-- `post_authors` joins two (PostgreSQL will not).
CREATE VIEW user_emails AS
    SELECT id, email FROM users;

CREATE VIEW post_authors AS
    SELECT p.id AS post_id, p.title AS title, u.id AS user_id, u.name AS user_name
    FROM posts p JOIN users u ON u.id = p.user_id;
