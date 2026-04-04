# benri-stream

## Service Context

**benri-stream** is a minimal private video streaming service. Users upload video files
anonymously (no accounts), the system transcodes them into adaptive-quality HLS streams,
and generates a shareable link. Anyone with the link can watch.

---

## Spec-Driven Development

The `.spec/` directory is the source of truth for design and implementation. Always read
the relevant spec before writing code.

```
.spec/
  architecture/
    system-architecture.md        <- system overview, stack, layers, patterns
    backend/
      workspace-crates.md         <- crate structure, dependency rules, source layout, config
      data-store.md               <- schema, repositories, migrations
      storage-layout.md           <- S3 structure, presigned URLs
      transcoding.md              <- pipeline, quality levels, GPU
      task-system.md              <- task entity, lifecycle, worker design
      error-handling.md           <- error strategy per layer
      testing.md                  <- test strategy, test layout
      observability.md            <- logging, tracing, metrics
  business-spec/                  <- WHAT to build
    SPEC_GUIDE.md                 <- how to write and update specs
    user-stories/
      anonymous-user.md
    video/
      video.md                    <- Video entity, use cases
      video.ui.md                 <- screens, interactions
    task-system/
      task-catalog.md             <- all background task types
```

**Workflow**: spec first -> implement -> test. Do not implement features that aren't spec'd.
Do not deviate from the spec without updating it first.

---

## Non-Negotiable Rules

For full architecture details, see the relevant `architecture/backend/` doc.

1. **Dependency direction**: `api/worker` -> `application` -> `domain` <- `infrastructure`.
   Application cannot import infrastructure -- compiler enforced via workspace crates.
2. **All fallible operations return `Result`**. Never panic for expected failures.
   `panic!` is only for programming bugs. See `backend/error-handling.md`.
3. **No business logic** in `infrastructure` or `presentation` layers.
4. **Use cases never call other use cases.** Shared logic lives in application services.
5. **All repository and port methods are `async`**.
6. **Transactions are owned by use cases** -- never by infrastructure or presentation.
7. **Error types are defined per use case**, not per entity.
8. **Tasks are created via `TaskScheduler`** within the same DB transaction as the
   triggering operation. See `backend/task-system.md`.

---

## Naming Conventions

| Kind | Pattern | Example |
|------|---------|---------|
| Use case | `[Verb][Subject]UseCase` | `InitiateUploadUseCase` |
| Application service | `[Concept]Service` | `VideoValidationService` |
| Port trait | `[Subject]Repository` / `[Concept]Port` | `VideoRepository`, `StoragePort` |
| Port implementation | `[Subject]RepositoryImpl` / `[Concept]Client` | `PostgresVideoRepository`, `S3StorageClient` |
| Use case error | Nested `Error` enum inside use case | `InitiateUploadUseCase::Error` |
| Task metadata | `[Action]TaskMetadata` | `ProcessVideoTaskMetadata` |
| Task handler | `[Action]TaskHandler` | `ProcessVideoTaskHandler` |
| Axum handler | `[verb]_[subject]` function | `initiate_upload`, `get_video_status` |
