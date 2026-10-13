//! opentui-diff library surface — the differential harness' reusable
//! halves. The binary (`src/main.rs`) is the CLI acceptance instrument;
//! the perf rig (`tools/bench`) links this crate to drive the SAME
//! staged scenario trees as benchmark fixtures.

pub mod compare;
pub mod scenario;
pub mod solid;
pub mod vt;
