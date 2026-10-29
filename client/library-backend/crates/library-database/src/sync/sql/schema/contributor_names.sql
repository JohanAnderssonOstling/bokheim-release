-- Disposable matching keys; original names remain on identities and credits.
CREATE TABLE IF NOT EXISTS author_name_key(
    name_key TEXT NOT NULL,
    author_identity_id INTEGER NOT NULL REFERENCES author_identity(id) ON DELETE CASCADE,
    PRIMARY KEY(name_key, author_identity_id)
) WITHOUT ROWID;
