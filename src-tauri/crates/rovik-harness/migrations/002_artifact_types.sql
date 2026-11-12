-- The same exact bytes can be source, a log or another registered artifact.
-- Type belongs to the reference; content identity remains the SHA-256 alone.
CREATE TABLE artifact_types(
 digest TEXT NOT NULL REFERENCES artifacts(digest), kind TEXT NOT NULL,
 media_type TEXT NOT NULL, PRIMARY KEY(digest, kind, media_type)
);
INSERT INTO artifact_types SELECT digest,kind,media_type FROM artifacts;
INSERT INTO schema_versions VALUES(2,0);
PRAGMA user_version=2;
