//! Platform-neutral core of Pulse.
//!
//! Nothing in this crate calls the operating system. A platform backend implements the traits in
//! [`platform`] and hands raw, cumulative counters to the [`engine`], which turns them into rates,
//! percentages, history and insights. Everything here is plain data and maths, so it is unit tested
//! without real hardware.

pub mod counter;
pub mod cpu;
pub mod engine;
pub mod format;
pub mod graph;
pub mod history;
pub mod insights;
pub mod model;
pub mod platform;
pub mod process;
pub mod safety;
pub mod settings;
pub mod table;

pub use model::*;
