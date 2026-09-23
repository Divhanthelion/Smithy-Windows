//! smithy-run — unattended Runs.
//!
//! One intent in, a branch of checked commits out. See `DESIGN.md` beside
//! this crate for the decisions and the order of authority the runner keeps:
//! Checks, then mechanical rules, then Jev, then the model.

pub mod check;
pub mod git;
pub mod plan;
pub mod toolchain;

pub use check::{run_check, CheckKind, CheckOutcome, CheckSpec};
pub use toolchain::{Counter, TestRun, Toolchain};
