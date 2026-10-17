use dbconnector_core::{DbError, ErrorCode};
use sqlparser::ast::{Query, SetExpr, Statement, Visit, Visitor};
use sqlparser::dialect::{MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

use crate::dialect::SqlDialect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Read,
    Write { destructive: bool },
}

pub fn classify(dialect: SqlDialect, sql: &str) -> Result<StatementKind, DbError> {
    let statement = parse_one(dialect, sql)?;
    if contains_blocked_routine(sql) {
        return Err(DbError::permission(
            "statement references a blocked file, extension, or admin routine",
        ));
    }
    classify_statement(&statement)
}

pub fn parse_one(dialect: SqlDialect, sql: &str) -> Result<Statement, DbError> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Err(DbError::invalid("sql is empty"));
    }
    let statements = parse_sql(dialect, sql)
        .map_err(|err| DbError::invalid(format!("SQL parse error: {err}")))?;
    if statements.len() != 1 {
        return Err(DbError::invalid("only a single SQL statement is allowed"));
    }
    Ok(statements.into_iter().next().expect("length checked"))
}

fn parse_sql(
    dialect: SqlDialect,
    sql: &str,
) -> Result<Vec<Statement>, sqlparser::parser::ParserError> {
    match dialect {
        SqlDialect::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql),
        SqlDialect::Mysql => Parser::parse_sql(&MySqlDialect {}, sql),
        SqlDialect::Sqlite => Parser::parse_sql(&SQLiteDialect {}, sql),
    }
}

fn classify_statement(statement: &Statement) -> Result<StatementKind, DbError> {
    match statement {
        Statement::Query(query) => {
            if query_has_side_effect(query) {
                return Err(DbError::permission(
                    "read-only SQL rejects locking reads and SELECT INTO",
                ));
            }
            Ok(StatementKind::Read)
        }
        Statement::Explain {
            analyze, statement, ..
        } => {
            if *analyze {
                return Err(DbError::permission(
                    "EXPLAIN ANALYZE executes the statement and is not enabled",
                ));
            }
            match classify_statement(statement)? {
                StatementKind::Read => Ok(StatementKind::Read),
                StatementKind::Write { .. } => Err(DbError::permission(
                    "EXPLAIN of a write statement is not allowed on the read path",
                )),
            }
        }
        Statement::ExplainTable { .. }
        | Statement::ShowCollation { .. }
        | Statement::ShowColumns { .. }
        | Statement::ShowCreate { .. }
        | Statement::ShowDatabases { .. }
        | Statement::ShowFunctions { .. }
        | Statement::ShowObjects(_)
        | Statement::ShowSchemas { .. }
        | Statement::ShowStatus { .. }
        | Statement::ShowTables { .. }
        | Statement::ShowVariable { .. }
        | Statement::ShowVariables { .. }
        | Statement::ShowViews { .. } => Ok(StatementKind::Read),
        Statement::Insert(_) => Ok(StatementKind::Write { destructive: false }),
        Statement::Update { selection, .. } => Ok(StatementKind::Write {
            destructive: selection.is_none(),
        }),
        Statement::Delete(delete) => Ok(StatementKind::Write {
            destructive: delete.selection.is_none(),
        }),
        Statement::Truncate { .. }
        | Statement::Drop { .. }
        | Statement::DropFunction { .. }
        | Statement::DropDomain(_)
        | Statement::DropProcedure { .. }
        | Statement::DropSecret { .. }
        | Statement::DropPolicy { .. }
        | Statement::DropConnector { .. }
        | Statement::DropExtension { .. }
        | Statement::DropTrigger { .. }
        | Statement::AlterTable { .. } => Ok(StatementKind::Write { destructive: true }),
        Statement::CreateTable(_)
        | Statement::CreateIndex(_)
        | Statement::CreateView { .. }
        | Statement::CreateSchema { .. }
        | Statement::CreateDatabase { .. } => Ok(StatementKind::Write { destructive: false }),
        Statement::StartTransaction { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::Set(_)
        | Statement::Use(_)
        | Statement::Grant { .. }
        | Statement::Call(_)
        | Statement::Execute { .. }
        | Statement::Copy { .. }
        | Statement::Pragma { .. } => Err(DbError::permission(
            "statement is not allowed through sql_query or sql_execute",
        )),
        _ => Err(DbError::new(
            ErrorCode::UnsupportedOperation,
            "statement type is not supported",
        )),
    }
}

fn query_has_side_effect(query: &Query) -> bool {
    let mut visitor = LockVisitor { bad: false };
    let _ = query.visit(&mut visitor);
    visitor.bad || !query.locks.is_empty()
}

struct LockVisitor {
    bad: bool,
}

impl Visitor for LockVisitor {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> std::ops::ControlFlow<Self::Break> {
        if !query.locks.is_empty() {
            self.bad = true;
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(
        &mut self,
        _expr: &sqlparser::ast::Expr,
    ) -> std::ops::ControlFlow<Self::Break> {
        std::ops::ControlFlow::Continue(())
    }
}

fn set_expr_has_into(expr: &SetExpr) -> bool {
    match expr {
        SetExpr::Select(select) => select.into.is_some(),
        SetExpr::Query(query) => set_expr_has_into(&query.body),
        SetExpr::SetOperation { left, right, .. } => {
            set_expr_has_into(left) || set_expr_has_into(right)
        }
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) => true,
        SetExpr::Values(_) | SetExpr::Table(_) => false,
    }
}

pub fn ensure_read(dialect: SqlDialect, sql: &str) -> Result<(), DbError> {
    let statement = parse_one(dialect, sql)?;
    if set_expr_of_query(&statement).is_some_and(set_expr_has_into) {
        return Err(DbError::permission("SELECT INTO is not allowed"));
    }
    match classify_statement(&statement)? {
        StatementKind::Read => {
            if contains_blocked_routine(sql) {
                return Err(DbError::permission(
                    "statement references a blocked file, extension, or admin routine",
                ));
            }
            Ok(())
        }
        StatementKind::Write { .. } => Err(DbError::permission(
            "sql_query only accepts a single read-only statement",
        )),
    }
}

fn set_expr_of_query(statement: &Statement) -> Option<&SetExpr> {
    match statement {
        Statement::Query(query) => Some(&query.body),
        Statement::Explain { statement, .. } => set_expr_of_query(statement),
        _ => None,
    }
}

pub fn ensure_execute(dialect: SqlDialect, sql: &str) -> Result<bool, DbError> {
    let kind = classify(dialect, sql)?;
    match kind {
        StatementKind::Read => Err(DbError::invalid(
            "sql_execute rejects read-only statements; use sql_query",
        )),
        StatementKind::Write { destructive } => Ok(destructive),
    }
}

fn contains_blocked_routine(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    const BLOCKED: &[&str] = &[
        "load_extension",
        "writefile",
        "readfile",
        "pg_read_file",
        "pg_read_binary_file",
        "pg_write_file",
        "pg_ls_dir",
        "lo_import",
        "lo_export",
        "into outfile",
        "into dumpfile",
    ];
    BLOCKED.iter().any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_select_and_rejects_stacked_statements() {
        ensure_read(SqlDialect::Postgres, "SELECT id FROM users WHERE id > $1").unwrap();
        let err = ensure_read(SqlDialect::Postgres, "SELECT 1; DROP TABLE users").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn rejects_dml_and_locking_reads() {
        let err = ensure_read(SqlDialect::Postgres, "DELETE FROM users WHERE id = 1").unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        let err = ensure_read(SqlDialect::Postgres, "SELECT * FROM users FOR UPDATE").unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
    }

    #[test]
    fn destructive_without_where() {
        let destructive = ensure_execute(SqlDialect::Mysql, "DELETE FROM users").unwrap();
        assert!(destructive);
        let destructive = ensure_execute(
            SqlDialect::Mysql,
            "UPDATE users SET name = 'a' WHERE id = 1",
        )
        .unwrap();
        assert!(!destructive);
        let destructive = ensure_execute(SqlDialect::Sqlite, "DROP TABLE users").unwrap();
        assert!(destructive);
    }

    #[test]
    fn explain_analyze_is_rejected_on_read_path() {
        let err = ensure_read(SqlDialect::Postgres, "EXPLAIN ANALYZE SELECT 1").unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
    }
}
