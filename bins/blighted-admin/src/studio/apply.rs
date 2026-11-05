// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Apply job (spec section 5.4): check, write the merged files, restart the world through
//! the control FIFO of `scripts/live-stack.sh`, and wait for every sim to start again.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::PoisonError;
use std::time::Duration;

use nw_common::time::unix_ms;
use nw_design::{DesignError, DesignMode, DesignReport, run_design};
use serde::Serialize;

use crate::live::lock;
use crate::studio::Studio;

/// The line that restarts the world (`scripts/live-stack.sh`).
const RESTART_LINE: &[u8] = b"restart-world\n";
/// Linux `O_NONBLOCK`: opening the FIFO fails at once when nothing reads it.
const OPEN_NONBLOCK: i32 = 0o4000;
/// Time between two looks at the sims while waiting.
const SIM_POLL: Duration = Duration::from_millis(250);

/// The step the job is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStep {
    /// The full design check.
    Check,
    /// Writing the merged files.
    Write,
    /// Telling the live stack to restart the world.
    Restart,
    /// Waiting for the sims.
    WaitSims,
}

/// Where the job is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPhase {
    /// No job ran yet.
    #[default]
    Idle,
    /// A job runs.
    Running,
    /// The last job finished.
    Done,
    /// The last job stopped on a problem.
    Failed,
}

/// The state of the Apply job, as the page shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ApplyStatus {
    /// Where the job is.
    pub phase: ApplyPhase,
    /// The step, while running or where it ended.
    pub step: Option<ApplyStep>,
    /// One line for the page.
    pub detail: String,
    /// The design problems that stopped the job.
    pub errors: Vec<DesignError>,
}

impl ApplyStatus {
    fn running(step: ApplyStep, detail: impl Into<String>) -> Self {
        Self::ended(ApplyPhase::Running, step, detail)
    }

    fn ended(phase: ApplyPhase, step: ApplyStep, detail: impl Into<String>) -> Self {
        Self {
            phase,
            step: Some(step),
            detail: detail.into(),
            errors: Vec::new(),
        }
    }
}

impl Studio {
    /// The state of the Apply job.
    pub fn apply_status(&self) -> ApplyStatus {
        self.apply
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_apply(&self, status: ApplyStatus) {
        *self.apply.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }

    /// Starts the Apply job on the current runtime; `false` while an Apply or a Check runs.
    pub fn start_apply(&self) -> bool {
        let Ok(permit) = self.work.clone().try_acquire_owned() else {
            return false;
        };
        self.set_apply(ApplyStatus::running(
            ApplyStep::Check,
            "checking the design files",
        ));
        let studio = self.clone();
        tokio::spawn(async move {
            let job = tokio::spawn({
                let studio = studio.clone();
                async move { studio.run_apply().await }
            });
            let end = job.await.unwrap_or_else(|error| studio.stopped(&error));
            studio.set_apply(end);
            drop(permit);
        });
        true
    }

    /// The end state of an Apply job that stopped with `error` (a panic) at its last step.
    pub(crate) fn stopped(&self, error: &tokio::task::JoinError) -> ApplyStatus {
        let step = self.apply_status().step.unwrap_or(ApplyStep::Check);
        ApplyStatus::ended(
            ApplyPhase::Failed,
            step,
            format!("the Apply job stopped: {error}"),
        )
    }

    /// The full check alone; `None` while an Apply or another Check runs.
    pub async fn check(&self) -> Option<Result<DesignReport, String>> {
        let _permit = self.work.try_acquire().ok()?;
        Some(self.design(DesignMode::Check).await)
    }

    /// The design step in `mode`, off the runtime threads.
    async fn design(&self, mode: DesignMode) -> Result<DesignReport, String> {
        let paths = self.paths.clone();
        let build = self.client_build.clone();
        tokio::task::spawn_blocking(move || run_design(&paths, &build, mode))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
    }

    /// Runs every step and returns the end state.
    async fn run_apply(&self) -> ApplyStatus {
        for (step, mode, detail) in [
            (
                ApplyStep::Check,
                DesignMode::Check,
                "checking the design files",
            ),
            (
                ApplyStep::Write,
                DesignMode::Write,
                "writing the merged files",
            ),
        ] {
            self.set_apply(ApplyStatus::running(step, detail));
            match self.design(mode).await {
                Err(error) => return ApplyStatus::ended(ApplyPhase::Failed, step, error),
                Ok(report) if !report.errors.is_empty() => {
                    return ApplyStatus {
                        detail: format!("{} problems in design files", report.errors.len()),
                        errors: report.errors,
                        ..ApplyStatus::ended(ApplyPhase::Failed, step, "")
                    };
                }
                Ok(_) => {}
            }
        }
        // The map reads the same merged files as the sims: files it cannot read stop the job
        // before the restart, so the running world keeps the files it started with in memory.
        if let Err(error) = self.reload_world().await {
            return ApplyStatus::ended(
                ApplyPhase::Failed,
                ApplyStep::Write,
                format!(
                    "merged files written, but the world does not read them: {error}; the world was not restarted"
                ),
            );
        }
        self.set_apply(ApplyStatus::running(
            ApplyStep::Restart,
            "restarting the world",
        ));
        if !self.control_fifo.exists() {
            return ApplyStatus::ended(
                ApplyPhase::Done,
                ApplyStep::Restart,
                "content written; no live stack control, restart the sims yourself",
            );
        }
        let ids = self
            .live
            .as_ref()
            .map(|live| lock(live).sim_ids())
            .unwrap_or_default();
        let restart_ms = unix_ms();
        if let Err(error) = write_restart(&self.control_fifo) {
            return ApplyStatus::ended(
                ApplyPhase::Failed,
                ApplyStep::Restart,
                format!("cannot write the live stack control: {error}"),
            );
        }
        let Some(live) = &self.live else {
            return ApplyStatus::ended(
                ApplyPhase::Done,
                ApplyStep::Restart,
                "world restarting; no live panel to watch the sims",
            );
        };
        let deadline = tokio::time::Instant::now() + self.restart_timeout;
        loop {
            // With no sim known before the restart (none had sent a heartbeat yet), the first
            // sim back ends the wait.
            let (back, detail) = if ids.is_empty() {
                let back = lock(live).started_since(restart_ms);
                (
                    back,
                    format!("{back} sims back; no sim was known before the restart"),
                )
            } else {
                let back = lock(live).started_after(&ids, restart_ms);
                (back, format!("{back} of {} sims back", ids.len()))
            };
            if back > 0 && back >= ids.len() {
                return ApplyStatus::ended(ApplyPhase::Done, ApplyStep::WaitSims, detail);
            }
            if tokio::time::Instant::now() >= deadline {
                return ApplyStatus::ended(
                    ApplyPhase::Failed,
                    ApplyStep::WaitSims,
                    format!(
                        "sims did not return in {} s ({detail})",
                        self.restart_timeout.as_secs()
                    ),
                );
            }
            self.set_apply(ApplyStatus::running(ApplyStep::WaitSims, detail));
            tokio::time::sleep(SIM_POLL).await;
        }
    }
}

/// Writes the restart line to the control FIFO; fails at once when nothing reads it.
fn write_restart(fifo: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(OPEN_NONBLOCK)
        .open(fifo)?
        .write_all(RESTART_LINE)
}
