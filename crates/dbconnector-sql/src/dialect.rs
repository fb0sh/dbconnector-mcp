#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlDialect {
    Postgres,
    Mysql,
    Sqlite,
}

impl SqlDialect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Sqlite => "sqlite",
        }
    }

    pub fn placeholder(self, index: usize) -> String {
        match self {
            Self::Postgres => format!("${}", index + 1),
            Self::Mysql | Self::Sqlite => "?".to_string(),
        }
    }
}
