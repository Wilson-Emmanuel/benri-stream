# Error Handling

Every fallible operation returns `Result<T, E>`. Errors are explicitly handled or propagated (`?`).

---

## Strategy by Layer

**Port traits (domain)** — all methods return `Result`, even operations like `find_by_id`. Each port defines its own error type (`RepositoryError`, `StorageError`, `TranscoderError`).

**Use cases (application)** — each use case defines its own error enum:
- Business errors — specific, expected (`VideoNotFound`, `TitleRequired`)
- `Internal(String)` — wraps unexpected port failures via `.map_err(|e| Error::Internal(e.to_string()))?`

**API handlers** — map use case errors to HTTP status codes: business errors to 400/404/409, `Internal` to 500.

**Worker handlers** — map use case errors to task outcomes: business errors to permanent failure, `Internal` to retryable failure.

**Panics** — reserved for programming bugs only. If it can happen in production, it's a `Result`.

---

## File Locations

| What | Where |
|------|-------|
| Port error types | `crates/domain/src/ports/error.rs` |
| Use case errors | Nested `Error` enum in each use case file under `crates/application/src/usecases/` |
| API error mapping | `crates/api/src/handlers/video.rs` (`impl From<Error> for ApiError`) |
| New use case error | Define in the use case file, add `From` impl in the handler |
