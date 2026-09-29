
CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS completed_chunk(path TEXT PRIMARY KEY,sha256 TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS profile(qid TEXT PRIMARY KEY,name TEXT,aliases TEXT,birth_year INTEGER,death_year INTEGER,identifiers TEXT,image TEXT,description TEXT);
CREATE TABLE IF NOT EXISTS isbn(isbn13 TEXT,book TEXT,PRIMARY KEY(isbn13,book));
CREATE TABLE IF NOT EXISTS credit(book TEXT,author TEXT,position INTEGER,PRIMARY KEY(book,author));
CREATE TABLE IF NOT EXISTS edition_work(book TEXT,work TEXT,PRIMARY KEY(book,work));
CREATE TABLE IF NOT EXISTS classification(book TEXT,scheme TEXT,notation TEXT,property TEXT,topic_scope INTEGER,source TEXT,PRIMARY KEY(book,scheme,notation,property,source));
CREATE TABLE IF NOT EXISTS topic_link(book TEXT,topic TEXT,PRIMARY KEY(book,topic));
CREATE TABLE IF NOT EXISTS ol_profile(author_id INTEGER PRIMARY KEY,aliases TEXT,birth_year INTEGER,death_year INTEGER);

CREATE TABLE IF NOT EXISTS isbn_author(isbn13 TEXT,book TEXT,work TEXT,author TEXT,source TEXT,PRIMARY KEY(isbn13,book,work,author));
