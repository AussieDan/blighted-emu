// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared configuration loading, logging setup, the wall clock, UDP sockets and the stop signal
//! for all emulator programs.

pub mod config;
pub mod logging;
pub mod net;
pub mod prometheus;
pub mod shutdown;
pub mod time;
