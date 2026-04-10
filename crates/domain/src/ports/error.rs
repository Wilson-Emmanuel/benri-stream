/// Error type shared across repository ports. Infrastructure adapters
/// convert their concrete client errors (e.g. `sqlx::Error`) into this
/// type at the port boundary.
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("database error: {0}")]
    Database(String),
}
