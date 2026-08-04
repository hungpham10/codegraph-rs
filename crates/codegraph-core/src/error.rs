use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("db: {0}")]
    Db(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("search: {0}")]
    Search(String),
    #[error("depth {depth} exceeds limit {limit}")]
    DepthExceedsLimit { depth: usize, limit: usize },
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("not initialized: run `codegraph init` first")]
    NotInitialized,
    #[error("{0}")]
    Other(String),
}
