//! smithy-run — unattended Runs.
//!
//! One intent in, a branch of checked commits out. See `DESIGN.md` beside
//! this crate for the decisions and the order of authority the runner keeps:
//! Checks, then mechanical rules, then Jev, then the model.

pub mod cheat;
pub mod check;
pub mod decisions;
pub mod git;
pub mod plan;
pub mod prompts;
pub mod report;
pub mod runner;
pub mod state;
pub mod toolchain;
pub mod unattended;

pub use check::{run_check, CheckKind, CheckOutcome, CheckSpec};
pub use plan::{Plan, Task};
pub use state::{Ceilings, RunState, TaskStatus, Verdict};
pub use toolchain::{Counter, TestRun, Toolchain};
