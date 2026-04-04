use redis::AsyncCommands;

pub struct DistributedLock {
    client: redis::Client,
}

impl DistributedLock {
    pub fn new(client: redis::Client) -> Self {
        Self { client }
    }

    pub async fn acquire(&self, key: &str, ttl_secs: u64) -> Result<bool, String> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| e.to_string())?;

        let result: bool = redis::cmd("SET")
            .arg(key)
            .arg("1")
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs)
            .query_async(&mut conn)
            .await
            .map_err(|e| e.to_string())?;

        Ok(result)
    }

    pub async fn release(&self, key: &str) -> Result<(), String> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| e.to_string())?;

        let _: () = conn.del(key).await.map_err(|e| e.to_string())?;
        Ok(())
    }
}
