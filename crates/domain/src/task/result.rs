#[derive(Debug, Clone)]
pub enum TaskResult {
    Success { message: Option<String> },
    RetryableFailure { error: String },
    PermanentFailure { error: String },
    Skip { reason: String },
}
