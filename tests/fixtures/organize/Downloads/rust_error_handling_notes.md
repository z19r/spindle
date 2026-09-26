# Rust error handling notes

- `?` converts via `From`, so one error enum per crate boundary
- `thiserror` for libraries (derive `Error`), `anyhow` for binaries
- `anyhow::Context::context` adds a frame without a new type
- Never `unwrap()` on anything a user can cause; `expect()` with a
  sentence saying what the caller broke
