//! Harness Wrangler core: controlled experiments over coding-agent harness
//! patterns. No UI dependency; the CLI and desktop app are thin wrappers.

// pub mod ablation;
// pub mod advice;
// pub mod api;
// pub mod graders;
// pub mod interop;
pub mod isolation;
pub mod model;
pub mod project;
// pub mod report;
// pub mod review;
pub mod runner;
// pub mod scheduler;
// pub mod stats;
pub mod store;
// pub mod suite;
pub mod transcript;
pub mod util;
pub mod workspace;

pub use model::*;
pub use project::Project;
pub use store::Store;
