// SPDX-License-Identifier: AGPL-3.0-or-later
//! The stop signal of every server program: Ctrl-C (SIGINT) in a terminal, and on Unix also
//! SIGTERM, the signal `kill`, systemd and Docker send to stop a program. A program that waits only
//! for Ctrl-C ends at once on SIGTERM and skips its last saves.

/// Returns when the program is told to stop: Ctrl-C, or SIGTERM on Unix.
///
/// # Errors
///
/// Returns the I/O error of a signal handler that cannot be installed.
pub async fn stop_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            interrupt = tokio::signal::ctrl_c() => interrupt,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sigterm_is_a_stop_signal() {
        // A SIGTERM before any handler would end the test process: this listener replaces the
        // default action first.
        let _guard = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("handler installed");
        let stopped = tokio::spawn(stop_signal());
        while !stopped.is_finished() {
            let pid = std::process::id().to_string();
            let sent = std::process::Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .expect("kill runs");
            assert!(sent.success());
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        stopped.await.expect("joined").expect("handler installed");
    }
}
