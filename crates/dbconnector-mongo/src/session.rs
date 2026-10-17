#![allow(clippy::too_many_arguments)]

use std::future::IntoFuture;
use std::time::Duration;

use bson::{doc, Document};
use dbconnector_core::{redact_secrets, DbError, ErrorCode};
use futures::TryStreamExt;
use mongodb::bson::Bson;
use mongodb::Client;
use serde_json::{json, Value};

use crate::extjson::{bson_to_json, document_to_json, json_to_document, pipeline_has_write_stage};

pub struct MongoLimits {
    pub max_docs: u64,
    pub max_bytes: usize,
    pub timeout: Duration,
}

pub struct MongoSession {
    client: Client,
    pub read_only: bool,
    pub server_version: String,
    pub default_database: Option<String>,
}

impl MongoSession {
    pub async fn connect(
        database_url: &str,
        read_only: bool,
        default_database: Option<String>,
    ) -> Result<Self, DbError> {
        let client = Client::with_uri_str(database_url)
            .await
            .map_err(connect_err)?;
        let ping = client
            .database("admin")
            .run_command(doc! {"ping": 1})
            .await
            .map_err(connect_err)?;
        let server_version = ping
            .get("ok")
            .map(|_| "unknown")
            .unwrap_or("unknown")
            .to_string();
        let server_version = match client
            .database("admin")
            .run_command(doc! {"buildInfo": 1})
            .await
        {
            Ok(info) => info.get_str("version").unwrap_or("unknown").to_string(),
            Err(_) => server_version,
        };
        Ok(Self {
            client,
            read_only,
            server_version,
            default_database,
        })
    }

    pub async fn list_databases(&self, limits: &MongoLimits) -> Result<Value, DbError> {
        let names = with_timeout(
            limits.timeout,
            self.client.list_database_names().into_future(),
        )
        .await?;
        Ok(json!({"databases": names}))
    }

    pub async fn list_collections(
        &self,
        database: &str,
        limits: &MongoLimits,
    ) -> Result<Value, DbError> {
        let db = self.client.database(database);
        let mut cursor = with_timeout(limits.timeout, db.list_collections().into_future()).await?;
        let mut collections = Vec::new();
        while let Some(spec) = cursor.try_next().await.map_err(query_err)? {
            collections.push(serde_json::to_value(&spec).unwrap_or(Value::Null));
            if collections.len() as u64 >= limits.max_docs {
                break;
            }
        }
        Ok(json!({"database": database, "collections": collections}))
    }

    pub async fn describe_collection(
        &self,
        database: &str,
        collection: &str,
        limits: &MongoLimits,
    ) -> Result<Value, DbError> {
        let coll = self
            .client
            .database(database)
            .collection::<Document>(collection);
        let mut options = Document::new();
        let listed = self.list_collections(database, limits).await?;
        if let Some(items) = listed.get("collections").and_then(Value::as_array) {
            for item in items {
                if item.get("name").and_then(Value::as_str) == Some(collection) {
                    options = json_to_document(item).unwrap_or_default();
                }
            }
        }
        let sample_limit = limits.max_docs.min(20);
        let mut cursor = with_timeout(
            limits.timeout,
            coll.find(Document::new())
                .limit(sample_limit as i64)
                .into_future(),
        )
        .await?;
        let mut samples = Vec::new();
        let mut fields = serde_json::Map::new();
        while let Some(doc) = cursor.try_next().await.map_err(query_err)? {
            for (key, value) in doc.iter() {
                fields.insert(key.to_string(), Value::String(bson_type_name(value).into()));
            }
            samples.push(document_to_json(&doc));
        }
        let indexes = self.list_indexes(database, collection, limits).await?;
        Ok(json!({
            "database": database,
            "collection": collection,
            "inferred": true,
            "sample_size": samples.len(),
            "fields": fields,
            "options": document_to_json(&options),
            "indexes": indexes.get("indexes").cloned().unwrap_or(Value::Array(vec![])),
        }))
    }

    pub async fn find(
        &self,
        database: &str,
        collection: &str,
        filter: &Value,
        projection: Option<&Value>,
        sort: Option<&Value>,
        limit: u64,
        skip: u64,
        limits: &MongoLimits,
    ) -> Result<Value, DbError> {
        let coll = self
            .client
            .database(database)
            .collection::<Document>(collection);
        let filter = json_to_document(filter)?;
        let mut find = coll
            .find(filter)
            .limit(limit.min(limits.max_docs) as i64)
            .skip(skip)
            .max_time(limits.timeout);
        if let Some(projection) = projection {
            find = find.projection(json_to_document(projection)?);
        }
        if let Some(sort) = sort {
            find = find.sort(json_to_document(sort)?);
        }
        let mut cursor = with_timeout(limits.timeout, find.into_future()).await?;
        let documents = collect_docs(&mut cursor, limits).await?;
        let returned = documents
            .get("items")
            .and_then(Value::as_array)
            .map(|items| items.len())
            .unwrap_or(0);
        Ok(json!({
            "database": database,
            "collection": collection,
            "documents": documents,
            "returned": returned,
        }))
    }

    pub async fn aggregate(
        &self,
        database: &str,
        collection: &str,
        pipeline: &[Value],
        allow_writes: bool,
        limits: &MongoLimits,
    ) -> Result<Value, DbError> {
        let docs: Result<Vec<_>, _> = pipeline.iter().map(json_to_document).collect();
        let docs = docs?;
        if pipeline_has_write_stage(&docs) && (self.read_only || !allow_writes) {
            return Err(DbError::permission(
                "aggregation write stages $out and $merge are rejected unless the server and connection allow writes",
            ));
        }
        let coll = self
            .client
            .database(database)
            .collection::<Document>(collection);
        let mut cursor = with_timeout(
            limits.timeout,
            coll.aggregate(docs).max_time(limits.timeout).into_future(),
        )
        .await?;
        let documents = collect_docs(&mut cursor, limits).await?;
        let returned = documents
            .get("items")
            .and_then(Value::as_array)
            .map(|items| items.len())
            .unwrap_or(0);
        Ok(json!({
            "database": database,
            "collection": collection,
            "documents": documents,
            "returned": returned,
        }))
    }

    pub async fn insert(
        &self,
        database: &str,
        collection: &str,
        documents: &[Value],
        limits: &MongoLimits,
    ) -> Result<Value, DbError> {
        self.ensure_write()?;
        if documents.is_empty() {
            return Err(DbError::invalid("documents must not be empty"));
        }
        if documents.len() as u64 > limits.max_docs {
            return Err(DbError::new(
                ErrorCode::ResourceLimitExceeded,
                "insert document count exceeds the limit",
            ));
        }
        let docs: Result<Vec<Document>, _> = documents.iter().map(json_to_document).collect();
        let coll = self
            .client
            .database(database)
            .collection::<Document>(collection);
        let result = with_timeout(limits.timeout, coll.insert_many(docs?).into_future()).await?;
        let ids: Vec<Value> = result
            .inserted_ids
            .into_values()
            .map(|id| bson_to_json(&id))
            .collect();
        Ok(json!({"inserted_count": ids.len(), "inserted_ids": ids}))
    }

    pub async fn update(
        &self,
        database: &str,
        collection: &str,
        filter: &Value,
        update: &Value,
        upsert: bool,
        multi: bool,
        allow_destructive: bool,
        server_allows_destructive: bool,
        limits: &MongoLimits,
    ) -> Result<Value, DbError> {
        self.ensure_write()?;
        let filter_doc = json_to_document(filter)?;
        if multi && filter_doc.is_empty() && !(allow_destructive && server_allows_destructive) {
            return Err(DbError::permission(
                "updating every document requires --allow-destructive and allow_destructive=true",
            ));
        }
        let update_doc = json_to_document(update)?;
        let coll = self
            .client
            .database(database)
            .collection::<Document>(collection);
        let value = if multi {
            let action = coll.update_many(filter_doc, update_doc).upsert(upsert);
            let result = with_timeout(limits.timeout, action.into_future()).await?;
            json!({
                "matched_count": result.matched_count,
                "modified_count": result.modified_count,
                "upserted_id": result.upserted_id.map(|id| bson_to_json(&id)),
            })
        } else {
            let action = coll.update_one(filter_doc, update_doc).upsert(upsert);
            let result = with_timeout(limits.timeout, action.into_future()).await?;
            json!({
                "matched_count": result.matched_count,
                "modified_count": result.modified_count,
                "upserted_id": result.upserted_id.map(|id| bson_to_json(&id)),
            })
        };
        Ok(value)
    }

    pub async fn delete(
        &self,
        database: &str,
        collection: &str,
        filter: &Value,
        multi: bool,
        allow_destructive: bool,
        server_allows_destructive: bool,
        limits: &MongoLimits,
    ) -> Result<Value, DbError> {
        self.ensure_write()?;
        let filter_doc = json_to_document(filter)?;
        if filter_doc.is_empty() && !(allow_destructive && server_allows_destructive) {
            return Err(DbError::permission(
                "deleting with an empty filter requires --allow-destructive and allow_destructive=true",
            ));
        }
        let coll = self
            .client
            .database(database)
            .collection::<Document>(collection);
        let deleted = if multi {
            with_timeout(limits.timeout, coll.delete_many(filter_doc).into_future())
                .await?
                .deleted_count
        } else {
            with_timeout(limits.timeout, coll.delete_one(filter_doc).into_future())
                .await?
                .deleted_count
        };
        Ok(json!({"deleted_count": deleted}))
    }

    pub async fn list_indexes(
        &self,
        database: &str,
        collection: &str,
        limits: &MongoLimits,
    ) -> Result<Value, DbError> {
        let coll = self
            .client
            .database(database)
            .collection::<Document>(collection);
        let mut cursor = with_timeout(limits.timeout, coll.list_indexes().into_future()).await?;
        let mut indexes = Vec::new();
        while let Some(index) = cursor.try_next().await.map_err(query_err)? {
            indexes.push(serde_json::to_value(&index).unwrap_or(Value::Null));
        }
        Ok(json!({"database": database, "collection": collection, "indexes": indexes}))
    }

    pub fn database_or_default<'a>(
        &'a self,
        requested: Option<&'a str>,
    ) -> Result<&'a str, DbError> {
        requested
            .filter(|name| !name.is_empty())
            .or(self.default_database.as_deref())
            .ok_or_else(|| {
                DbError::invalid(
                    "database is required when the connection URL has no default database",
                )
            })
    }

    fn ensure_write(&self) -> Result<(), DbError> {
        if self.read_only {
            Err(DbError::permission(
                "this connection is read-only; open a new connection with read_only=false",
            ))
        } else {
            Ok(())
        }
    }
}

async fn collect_docs(
    cursor: &mut mongodb::Cursor<Document>,
    limits: &MongoLimits,
) -> Result<Value, DbError> {
    let mut docs = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    while let Some(doc) = cursor.try_next().await.map_err(query_err)? {
        if docs.len() as u64 >= limits.max_docs {
            truncated = true;
            break;
        }
        let value = document_to_json(&doc);
        let size = serde_json::to_vec(&value).map(|v| v.len()).unwrap_or(0);
        if bytes + size > limits.max_bytes {
            truncated = true;
            break;
        }
        bytes += size;
        docs.push(value);
    }
    Ok(json!({"items": docs, "truncated": truncated}))
}

fn bson_type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Array(_) => "array",
        Bson::Document(_) => "object",
        Bson::Boolean(_) => "bool",
        Bson::Null => "null",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Decimal128(_) => "decimal128",
        Bson::Binary(_) => "binary",
        Bson::Timestamp(_) => "timestamp",
        _ => "other",
    }
}

async fn with_timeout<T>(
    timeout: Duration,
    fut: impl std::future::Future<Output = mongodb::error::Result<T>>,
) -> Result<T, DbError> {
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(query_err(err)),
        Err(_) => Err(DbError::new(
            ErrorCode::QueryTimeout,
            "MongoDB operation exceeded the timeout; execution state may be unknown",
        )),
    }
}

fn connect_err(err: mongodb::error::Error) -> DbError {
    DbError::new(
        ErrorCode::ConnectionFailed,
        redact_secrets(&err.to_string()),
    )
}

fn query_err(err: mongodb::error::Error) -> DbError {
    DbError::new(ErrorCode::QueryFailed, redact_secrets(&err.to_string()))
}
