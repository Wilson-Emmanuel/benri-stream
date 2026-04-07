# Error Handling

Rust has no exceptions. Every fallible operation returns `Result<T, E>` and errors must
be explicitly handled or propagated (`?` operator).

---

## Strategy by Layer

### Port traits (domain)

All port methods return `Result` — even operations like `find_by_id` that "shouldn't
fail." The type system forces acknowledgment of DB failures, network issues, etc.

Each port defines its own error type (e.g., `RepositoryError`, `StorageError`,
`TranscoderError`).

### Use case errors (application)

Each use case defines its own error enum with two kinds of variants:

- **Business errors** — specific, expected failures the caller handles distinctly
  (e.g., `VideoNotFound`, `TitleRequired`)
- **Infrastructure catch-all** — `Internal(String)` wraps unexpected port failures.
  Mapped via `.map_err(|e| Error::Internal(e.to_string()))?`

### Presentation layer

API handlers map use case errors to HTTP responses:
- Business errors -> specific status codes (400, 404, 409)
- `Internal` -> 500

Worker handlers map use case errors to task outcomes:
- Business errors -> permanent failure (dead letter)
- `Internal` -> potential retry

### Panics

Reserved for programming bugs only — broken invariants, unreachable code. If it can
happen in production, it's a `Result`, not a `panic!`.

---

## File Locations

| What | Crate | Path |
|------|-------|------|
| Shared port error type (`RepositoryError`) | `domain` | `src/ports/error.rs` |
| Port-specific error types (e.g., `StorageError`, `TranscoderError`) | `domain` | `src/ports/*.rs` (alongside each port trait) |
| Use case error enums | `application` | `src/usecases/*/[use_case].rs` (nested in each use case) |
| HTTP error mapping | `api` | `src/handlers/*.rs` (in each handler function) |
| Task result mapping | `worker` | `src/handlers/*_handler.rs` (in each task handler) |
