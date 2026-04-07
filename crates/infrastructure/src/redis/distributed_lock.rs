use async_trait::async_trait;
use uuid::Uuid;

use domain::ports::distributed_lock::{DistributedLockPort, LockError, LockToken};

/// Redis-backed distributed lock with ownership tokens.
///
/// Acquire sets the key via `SET NX EX` with a freshly generated UUID token.
/// Release runs a Lua check-and-delete script that only deletes the key if
/// the stored value still matches the token. This prevents a caller from
/// accidentally releasing another worker's lock after their own TTL expired.
pub struct RedisDistributedLock {
    client: redis::Client,
}

impl RedisDistributedLock {
    pub fn new(client: redis::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DistributedLockPort for RedisDistributedLock {
    async fn acquire(
        &self,
        key: &str,
        ttl_secs: u64,
    ) -> Result<Option<LockToken>, LockError> {
        tracing::debug!(key, ttl_secs, "lock: acquire attempt");
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| LockError::Internal(e.to_string()))?;

        let token = Uuid::new_v4().to_string();
        let set_result: Option<String> = redis::cmd("SET")
            .arg(key)
            .arg(&token)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs)
            .query_async(&mut conn)
            .await
            .map_err(|e| LockError::Internal(e.to_string()))?;

        Ok(set_result.and_then(|s| if s == "OK" { Some(LockToken(token)) } else { None }))
    }

    async fn release(&self, key: &str, token: &LockToken) -> Result<(), LockError> {
        tracing::debug!(key, "lock: release");
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| LockError::Internal(e.to_string()))?;

        let script = r#"
            if redis.call("get", KEYS[1]) == ARGV[1] then
                return redis.call("del", KEYS[1])
            else
                return 0
            end
        "#;

        let deleted: i64 = redis::Script::new(script)
            .key(key)
            .arg(&token.0)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| LockError::Internal(e.to_string()))?;

        if deleted == 0 {
            // The key was missing or the stored value did not match the
            // supplied token. Either the TTL expired and Redis evicted
            // the key, or another holder re-acquired it after expiry.
            // Both indicate the TTL is too short relative to the work
            // done while the lock was held.
            tracing::warn!(
                key,
                "lock: release no-op — token mismatch (TTL likely expired)",
            );
        }

        Ok(())
    }
}
