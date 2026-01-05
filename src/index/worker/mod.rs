//! Worker subprocess pool for parallel Nix evaluations.
//!
//! This module provides a worker pool that spawns multiple subprocesses,
//! each running its own Nix evaluator. This enables parallel evaluation
//! of different systems (e.g., x86_64-linux, aarch64-darwin) for the
//! same commit, bypassing the Nix C API's single-threaded limitation.
//!
//! # Architecture
//!
//! ```text
//!                     ┌─────────────────┐
//!                     │  Parent Process │
//!                     │   (Indexer)     │
//!                     └────────┬────────┘
//!                              │
//!               ┌──────────────┼──────────────┐
//!               │              │              │
//!         ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐
//!         │ Worker 1  │  │ Worker 2  │  │ Worker N  │
//!         │ (process) │  │ (process) │  │ (process) │
//!         │ NixEval   │  │ NixEval   │  │ NixEval   │
//!         └───────────┘  └───────────┘  └───────────┘
//! ```
//!
//! # Features
//!
//! - **Process isolation**: Each worker has its own memory space
//! - **Memory recycling**: Workers restart when memory exceeds threshold
//! - **Crash recovery**: Failed workers are automatically respawned
//! - **Platform support**: Fork on Linux, posix_spawn on macOS

mod ipc;
mod pool;
mod proc;
mod protocol;
mod signals;
mod spawn;
pub mod worker_main;

pub use pool::{WorkerPool, WorkerPoolConfig};
pub use worker_main::run_worker_main;

// Re-export for potential external use
#[allow(unused_imports)]
pub use protocol::{WorkRequest, WorkResponse};
