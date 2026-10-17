CREATE TABLE notes (
    id integer PRIMARY KEY,
    body text NOT NULL
);
INSERT INTO notes (id, body) VALUES (1, 'fixture');

CREATE USER dbconnector_ro WITH PASSWORD 'dbconnector_test';
GRANT CONNECT ON DATABASE app TO dbconnector_ro;
GRANT USAGE ON SCHEMA public TO dbconnector_ro;
GRANT SELECT ON notes TO dbconnector_ro;
