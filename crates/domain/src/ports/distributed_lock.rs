use async_trait::async_trait;

/// Distributed lock for serializing periodic worker components across
/// multiple instances. Implementations must be ownership-checked: a release
/// with a stale token is a no-op, not a delete of the current holder's lock.
#[async_trait]
pub trait DistributedLockPort: Send + Sync {
    /// Attempt to acquire the lock with a TTL. Returns `Some(token)` on
    /// success, `None` if another holder owns the key.
    async fn acquire(
        &self,
        key: &str,
        ttl_secs: u64,
    ) -> Result<Option<LockToken>, LockError>;

    /// Release the lock. No-op if `token` does not match the current holder
    /// (e.g. our TTL expired and another acquirer took the key).
    async fn release(&self, key: &str, token: &LockToken) -> Result<(), LockError>;
}

/// Opaque ownership token returned by `acquire`. The implementation chooses
/// the format (e.g. UUID) — callers must pass it back to `release`.
#[derive(Debug, Clone)]
pub struct LockToken(pub String);

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("lock error: {0}")]
    Internal(String),
}
