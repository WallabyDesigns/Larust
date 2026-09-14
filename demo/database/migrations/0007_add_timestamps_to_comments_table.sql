-- SQLite requires a DEFAULT when adding a NOT NULL column to a non-empty
-- table - 0 (Unix epoch) is a real, honest "unknown/legacy" value for any
-- row that already existed before this migration ran; every row created
-- after it gets a real value from `Comment`'s own `#[timestamps]`.
ALTER TABLE comments ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE comments ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
