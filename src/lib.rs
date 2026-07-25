//! A lightweight Windows user-session background sync utility with block-level delta synchronization.
//!
//! This crate exposes the core modules used by the syncdir binary.

pub mod config;
pub mod db;
pub mod error;
pub mod monitor;
pub mod startup;
pub mod sync;
pub mod tray;

/// Copyright notice for syncdir.
pub const COPYRIGHT: &str = "(c) 2026 Wendell Saligan";
