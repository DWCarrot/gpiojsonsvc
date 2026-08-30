//! GPIO module: libgpiod-derived traits and backend implementations.

pub mod libgpiod;
pub mod mock;
pub mod sys;

pub use libgpiod::*;
