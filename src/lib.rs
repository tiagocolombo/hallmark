//! Hallmark: a local, agent-first development loop for any git repository.
//!
//! The binary is `hall`. Agents are its main users, so every command
//! follows one contract (see `output.rs` and `error.rs`): JSON when stdout
//! isn't a terminal, a `next` list in every reply, errors as data with
//! documented exit codes.

pub mod assayer;
pub mod budget;
pub mod ci;
pub mod cli;
pub mod commands;
pub mod config;
pub mod daemon;
pub mod delivery;
pub mod detect;
pub mod error;
pub mod event;
pub mod findings;
pub mod fixer;
pub mod gate;
pub mod gh;
pub mod git;
pub mod hooks;
pub mod index;
pub mod intake;
pub mod item;
pub mod journal;
pub mod lease;
pub mod library;
pub mod notify;
pub mod now;
pub mod output;
pub mod paths;
pub mod plugin;
pub mod repo;
pub mod resume;
pub mod runner;
pub mod settings;
pub mod shipper;
pub mod stamp;
pub mod state;
pub mod tui;
pub mod usage;
pub mod watchman;
pub mod worker;

pub use cli::run;
