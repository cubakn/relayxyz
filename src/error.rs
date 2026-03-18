use std::fmt;

#[derive(Debug)]
pub enum RelayError {
    Json(serde_json::Error),
    Database(redb::DatabaseError),
    DbTable(redb::TableError),
    DbTransaction(Box<redb::TransactionError>),
    DbStorage(redb::StorageError),
    DbCommit(redb::CommitError),
    Hyper(hyper::Error),
    Http(hyper::http::Error),
    Ws(Box<tungstenite::Error>),
    Hex(hex::FromHexError),
    InvalidEvent(String),
    Rejected(String),
}

impl fmt::Display for RelayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "json: {e}"),
            Self::Database(e) => write!(f, "db: {e}"),
            Self::DbTable(e) => write!(f, "db table: {e}"),
            Self::DbTransaction(e) => write!(f, "db transaction: {e}"),
            Self::DbStorage(e) => write!(f, "db storage: {e}"),
            Self::DbCommit(e) => write!(f, "db commit: {e}"),
            Self::Hyper(e) => write!(f, "hyper: {e}"),
            Self::Http(e) => write!(f, "http: {e}"),
            Self::Ws(e) => write!(f, "ws: {e}"),
            Self::Hex(e) => write!(f, "hex: {e}"),
            Self::InvalidEvent(msg) => write!(f, "invalid event: {msg}"),
            Self::Rejected(msg) => write!(f, "rejected: {msg}"),
        }
    }
}

impl std::error::Error for RelayError {}

impl From<serde_json::Error> for RelayError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<redb::DatabaseError> for RelayError {
    fn from(e: redb::DatabaseError) -> Self {
        Self::Database(e)
    }
}
impl From<redb::TableError> for RelayError {
    fn from(e: redb::TableError) -> Self {
        Self::DbTable(e)
    }
}
impl From<redb::TransactionError> for RelayError {
    fn from(e: redb::TransactionError) -> Self {
        Self::DbTransaction(Box::new(e))
    }
}
impl From<redb::StorageError> for RelayError {
    fn from(e: redb::StorageError) -> Self {
        Self::DbStorage(e)
    }
}
impl From<redb::CommitError> for RelayError {
    fn from(e: redb::CommitError) -> Self {
        Self::DbCommit(e)
    }
}
impl From<hyper::Error> for RelayError {
    fn from(e: hyper::Error) -> Self {
        Self::Hyper(e)
    }
}
impl From<hyper::http::Error> for RelayError {
    fn from(e: hyper::http::Error) -> Self {
        Self::Http(e)
    }
}
impl From<tungstenite::Error> for RelayError {
    fn from(e: tungstenite::Error) -> Self {
        Self::Ws(Box::new(e))
    }
}
impl From<hex::FromHexError> for RelayError {
    fn from(e: hex::FromHexError) -> Self {
        Self::Hex(e)
    }
}
