# Repository Guidelines

## Project Structure

`src/` contains the Rust service. The application and configuration are in `app.rs` and `config.rs`; protocol handling, session logic, scheduling, transport, and GPIO backends are organized into modules. `src/_archived/` holds older implementations. Integration tests are in `tests/`; Python debug clients and their tests are in `tools/`. Protocol, architecture, and mock chip documentation is in `docs/`. Sample Rock5B configuration and GPIO data are in `assets/rock5b/`; the systemd unit is in `systemd/`.

## Build, Test, and Development

- `cargo build` builds the service.
- `cargo run -- --mock /path/to/config.toml` starts it with the mock backend.
- `cargo fmt` formats Rust code.
- `cargo test` runs Rust unit and integration tests.
- `python3 tools/test_debug_client.py` runs the debug client’s Python tests.

The real GPIO backend is not currently available; use `--mock` for local runs.

## Style and Naming

Use standard Rust formatting with `cargo fmt`. Follow Rust naming conventions: `snake_case` for modules, functions, and variables; `UpperCamelCase` for types and traits. Keep code in the existing module structure and follow nearby patterns when extending protocol or backend behavior.

## Testing

Add Rust tests alongside the module under test or in `tests/` for integration behavior. Name tests for the behavior they verify, such as `rejects_unknown_pin`. Run `cargo test` after Rust changes and the Python test command after changing the debug client. Use the mock backend and fixtures for GPIO behavior that does not require hardware.

## Commits and Pull Requests

Recent commits use short, imperative summaries, sometimes with a scope or feature prefix (for example, `implement c-compatible interface...` or `new feature: ...`). Keep subjects concise and action-oriented. Pull requests should explain behavior changes, note configuration or protocol impacts, link related issues when available, and include test commands and results. Add screenshots or examples when they help explain a user-facing change.

## Configuration and Protocol

Configuration and protocol details are documented in `README.md` and `docs/`. Keep them and relevant sample files aligned with changes to configuration, requests, or responses. Pin keys are exact strings; do not infer chip or line identifiers from their spelling.
