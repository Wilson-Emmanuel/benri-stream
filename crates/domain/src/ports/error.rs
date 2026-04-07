/// Generic data-store error returned by repository ports
/// (`VideoRepository`, `TaskRepository`, `TransactionPort`, etc.).
///
/// Lives in a shared module so the same type can be used by multiple
/// port traits without one trait module appearing to "own" it.
/// Infrastructure adapters convert their concrete client errors
/// (e.g. `sqlx::Error`) into this type before returning them across
/// the port boundary.
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("database error: {0}")]
    Database(String),
}
