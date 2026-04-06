use uuid::Uuid;

/// Redis-backed distributed lock with ownership tokens.
///
/// Acquire sets the key via `SET NX EX` with a freshly generated UUID token.
/// Release runs a Lua check-and-delete script that only deletes the key if
/// the stored value still matches the token. This prevents a caller from
/// accidentally releasing another worker's lock after their own TTL expired.
pub struct DistributedLock {
    client: redis::Client,
}

/// Opaque ownership token returned by `DistributedLock::acquire`. Callers
/// must pass it back to `release` to release the lock they acquired.
#[derive(Debug, Clone)]
pub struct LockToken(String);

impl DistributedLock {
    pub fn new(client: redis::Client) -> Self {
        Self { client }
    }

    /// Attempt to acquire the lock. Returns `Some(token)` on success,
    /// `None` if another holder has the lock.
    pub async fn acquire(
        &self,
        key: &str,
        ttl_secs: u64,
    ) -> Result<Option<LockToken>, String> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| e.to_string())?;

        let token = Uuid::new_v4().to_string();
        // SET key token NX EX ttl — returns "OK" on success, nil if key exists.
        let set_result: Option<String> = redis::cmd("SET")
            .arg(key)
            .arg(&token)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs)
            .query_async(&mut conn)
            .await
            .map_err(|e| e.to_string())?;

        Ok(set_result.and_then(|s| if s == "OK" { Some(LockToken(token)) } else { None }))
    }

    /// Release the lock. No-op (returns silently) if the token doesn't
    /// match the current holder — this is the correct behavior for TTL
    /// expiration followed by another acquirer.
    pub async fn release(&self, key: &str, token: &LockToken) -> Result<(), String> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| e.to_string())?;

        // Compare-and-delete via Lua. Returns 1 if deleted, 0 if token mismatch.
        let script = r#"
            if redis.call("get", KEYS[1]) == ARGV[1] then
                return redis.call("del", KEYS[1])
            else
                return 0
            end
        "#;

        let _: i64 = redis::Script::new(script)
            .key(key)
            .arg(&token.0)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| e.to_string())?;

        Ok(())
    }
}
