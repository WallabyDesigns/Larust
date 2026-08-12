CREATE TABLE tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE post_tag (
    post_id INTEGER NOT NULL REFERENCES posts(id),
    tag_id INTEGER NOT NULL REFERENCES tags(id),
    PRIMARY KEY (post_id, tag_id)
);
