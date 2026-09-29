PRAGMA auto_vacuum=FULL;
        PRAGMA foreign_keys=ON;
        CREATE TABLE IF NOT EXISTS source (
            hash TEXT PRIMARY KEY, checksum TEXT NOT NULL, size INTEGER NOT NULL CHECK(size>0)
        );
        CREATE TABLE IF NOT EXISTS block (
            hash TEXT NOT NULL REFERENCES source(hash) ON DELETE CASCADE,
            offset INTEGER NOT NULL, bytes BLOB NOT NULL, used INTEGER NOT NULL,
            PRIMARY KEY(hash, offset)
        );
        CREATE INDEX IF NOT EXISTS block_lru ON block(used);
        CREATE TABLE IF NOT EXISTS clock (id INTEGER PRIMARY KEY CHECK(id=1), tick INTEGER NOT NULL);
        INSERT OR IGNORE INTO clock VALUES(1,0);