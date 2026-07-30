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
