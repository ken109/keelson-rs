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
