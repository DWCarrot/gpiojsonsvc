//! Deferred `libgpiod-sys` FFI backend.
//!
//! Implement [`Backend`](super::libgpiod::Backend) and `Drop` on each `*Impl` concrete type.
//! See `.cursor/redesign/gpio-backend.md` and `.cursor/redesign/libgpiod-v2-api.md`.
