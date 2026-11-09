// SPDX-License-Identifier: GPL-3.0-or-later
//! `launcher`: the player launcher window.

use std::process::ExitCode;

use eframe::egui;
use launcher::app::LauncherApp;
use launcher::install_crypto_provider;
use launcher::settings::{Settings, default_settings_path};
use launcher::steam;
use launcher::theme;

/// Window title.
const APP_NAME: &str = "Blighted New World launcher";
/// Window size at start (wide enough for the play column beside the news), and the smallest
/// size (one column).
const WINDOW_SIZE: [f32; 2] = [1080.0, 720.0];
const MIN_WINDOW_SIZE: [f32; 2] = [420.0, 520.0];

fn main() -> ExitCode {
    install_crypto_provider();
    let path = match default_settings_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut settings = match Settings::load(&path) {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let lookup = |name: &str| std::env::var(name).ok();
    settings.options.fill_from_env(lookup);
    let found = steam::scan(lookup);
    settings.options.fill_from_scan(&found);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size(MIN_WINDOW_SIZE),
        ..Default::default()
    };
    let result = eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |creation| {
            theme::install(&creation.egui_ctx, settings.theme);
            Ok(Box::new(LauncherApp::new(path, settings, found)))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
