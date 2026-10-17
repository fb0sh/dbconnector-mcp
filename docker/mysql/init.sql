CREATE TABLE notes (
    id INT PRIMARY KEY,
    body TEXT NOT NULL
);
INSERT INTO notes (id, body) VALUES (1, 'fixture');

CREATE USER 'dbconnector_ro'@'%' IDENTIFIED BY 'dbconnector_test';
GRANT SELECT ON app.* TO 'dbconnector_ro'@'%';
FLUSH PRIVILEGES;
