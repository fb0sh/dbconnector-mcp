const app = db.getSiblingDB("app");
app.createUser({
  user: "dbconnector",
  pwd: "dbconnector_test",
  roles: [{ role: "readWrite", db: "app" }],
});
app.createUser({
  user: "dbconnector_ro",
  pwd: "dbconnector_test",
  roles: [{ role: "read", db: "app" }],
});
app.notes.insertOne({ body: "fixture" });
