CREATE TABLE IF NOT EXISTS asset_file(name TEXT PRIMARY KEY, physical_name TEXT NOT NULL, byte_length INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS asset_file_physical ON asset_file(physical_name); CREATE TABLE IF NOT EXISTS asset_revision(physical_name TEXT PRIMARY KEY, content_hash BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS retained_provider_upload(name TEXT PRIMARY KEY, metadata BLOB NOT NULL CHECK(length(metadata)<=8388608));
CREATE TABLE IF NOT EXISTS reference_selection(name TEXT PRIMARY KEY NOT NULL, filename TEXT NOT NULL);
