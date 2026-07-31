CREATE TABLE users (
    id         INT PRIMARY KEY,
    name       VARCHAR(255) NOT NULL,
    email      VARCHAR(255),
    age        INT,
    is_active  TINYINT(1) NOT NULL DEFAULT 1,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE posts (
    id           INT PRIMARY KEY,
    user_id      INT NOT NULL,
    title        VARCHAR(255) NOT NULL,
    status       VARCHAR(64),
    views        INT NOT NULL DEFAULT 0,
    published_at DATETIME,
    -- MySQL-only: MATCH (title) AGAINST (…) is refused outright without a FULLTEXT
    -- index (ERROR 1191), so the engine tier could not judge full-text search at all
    -- without this. It is inert for every other statement.
    FULLTEXT KEY posts_title_ft (title),
    FOREIGN KEY (user_id) REFERENCES users (id)
);

CREATE TABLE comments (
    id         INT PRIMARY KEY,
    post_id    INT NOT NULL,
    user_id    INT,
    body       TEXT NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (post_id) REFERENCES posts (id),
    FOREIGN KEY (user_id) REFERENCES users (id)
);

CREATE TABLE tags (
    id   INT PRIMARY KEY,
    name VARCHAR(255) NOT NULL UNIQUE
);

CREATE TABLE post_tags (
    post_id INT NOT NULL,
    tag_id  INT NOT NULL,
    PRIMARY KEY (post_id, tag_id),
    FOREIGN KEY (post_id) REFERENCES posts (id),
    FOREIGN KEY (tag_id) REFERENCES tags (id)
);

-- The mutually referencing pair — see the note in tests/schema/psql.sql for
-- what it is for. MySQL resolves a foreign key's target at DDL time too, so the
-- second constraint is likewise added after both tables exist.
CREATE TABLE threads (
    id               INT PRIMARY KEY,
    title            VARCHAR(255) NOT NULL,
    first_message_id INT
);

CREATE TABLE messages (
    id        INT PRIMARY KEY,
    thread_id INT NOT NULL,
    body      TEXT NOT NULL,
    FOREIGN KEY (thread_id) REFERENCES threads (id)
);

ALTER TABLE threads ADD CONSTRAINT threads_first_message_id_fk
    FOREIGN KEY (first_message_id) REFERENCES messages (id);

-- Two views — see the note in tests/schema/psql.sql. MySQL answers the
-- updatability question with a single `IS_UPDATABLE` flag it computes from the
-- view body, and it answers it for both of these.
CREATE VIEW user_emails AS
    SELECT id, email FROM users;

CREATE VIEW post_authors AS
    SELECT p.id AS post_id, p.title AS title, u.id AS user_id, u.name AS user_name
    FROM posts p JOIN users u ON u.id = p.user_id;
