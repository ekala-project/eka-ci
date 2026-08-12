# Agent Guidelines for EKA-CI

This document provides essential guidelines for AI agents working on the eka-ci codebase.

## Project Structure

```
eka-ci/
├── backend/          # Rust workspace (Cargo workspace)
│   └── server/       # Main server crate
└── frontend/         # Elm project
```

**backend/**: Actor-based message-passing CI system built in Rust
**frontend/**: Web UI built in Elm

## Backend Development (Rust)

### Code Organization Standards

**File Size Limit**: Maximum **500 lines** per file
- If a file exceeds 500 lines, split it into logical modules
- Use `mod.rs` to organize related functionality

**Function Size Limit**: Maximum **50 lines** per function
- If a function exceeds 50 lines, extract helper functions
- Each function should have a single, clear responsibility

### Development Workflow

**During Iteration:**
```bash
cargo check
```
- Run `cargo check` frequently while making changes
- Fix errors incrementally before proceeding
- Faster than `cargo build` for validation

**Before Completion:**
```bash
cargo check  # Must pass with 0 warnings
cargo fmt    # Format all code
```
- All warnings must be resolved (0 warnings required)
- Always run `cargo fmt` to ensure consistent formatting
- Consider running `cargo clippy` for additional lints

### Error Handling Requirements

**NEVER** use `let _ = <Result expr>;`

**Bad:**
```rust
let _ = some_function_returning_result();  // ❌ FORBIDDEN
let _ = file.write_all(data);              // ❌ FORBIDDEN
```

**Good:**
```rust
some_function_returning_result()?;                    // ✅ Propagate error
some_function_returning_result().ok();                // ✅ Explicit ignore (if justified)
if let Err(e) = some_function_returning_result() {    // ✅ Handle error
    log::warn!("Operation failed: {}", e);
}
```

**All `Result` types must be explicitly handled:**
- Propagate with `?`
- Handle with pattern matching
- Explicitly convert to `Option` with `.ok()` (if ignoring is justified)
- Log errors appropriately

### Additional Rust Conventions

- Use `log::debug!`, `log::info!`, `log::warn!`, `log::error!` for logging
- Prefer `tokio::spawn` for async tasks in actor services
- Use `Arc` for shared immutable data across tasks
- Use `Arc<DrvId>` pattern extensively (see existing code)
- Follow existing message-passing patterns (see `backend/docs/message_flow.md`)

## Frontend Development (Elm)

- Elm compilation must succeed with no errors
- Follow standard Elm architecture patterns
- See frontend-specific documentation for details

## Testing

- Run tests before submitting changes:
  ```bash
  cargo test
  ```
- Ensure existing tests pass
- Add tests for new functionality

## Documentation

- Document new services in `backend/docs/message_flow.md`
- Update architectural docs when adding message types
- Use inline documentation for complex algorithms

## Quick Reference

**Pre-commit Checklist:**
- [ ] `cargo check` passes with 0 warnings
- [ ] `cargo fmt` has been run
- [ ] No `let _ = <Result>;` patterns exist
- [ ] Files are < 500 lines
- [ ] Functions are < 50 lines
- [ ] All errors are properly handled
- [ ] Tests pass (`cargo test`)

---

**For questions about architecture**, see:
- `backend/docs/message_flow.md` - Message passing and service architecture
- Existing code patterns in `backend/server/src/`
