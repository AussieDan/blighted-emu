// SPDX-License-Identifier: GPL-3.0-or-later
//! The launcher window (M2 spec section 6). Network calls run on a worker thread; the window
//! reads their results each frame. The window shows one status line (a state bar, a
//! state word and a detail line), then the server, account, options and settings sections
//! divided by rules.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Sense, TextStyle, vec2};
use nw_protocol::launcher_api::{AuthMode, DiscordPollReply, LauncherInfo, PasswordCredentials};

use crate::api::{ApiError, LauncherApi, discord_login_page};
use crate::game::{PATCHED_EXE_NAME, append_override_config, game_arguments, is_retail_prefix};
// Linux runs the game under Proton inside `bwrap`; Windows starts the patched exe directly. Both
// run the loopback proxy ([`crate::aws_proxy`]) for a client patched with `--aws-proxy`. Each
// path uses only its own launch helpers.
use crate::aws_proxy::AwsProxy;
#[cfg(not(windows))]
use crate::game::{checked_direct_launch, start_game};
#[cfg(any(windows, test))]
use crate::game::{checked_windows_launch, start_game_windows};
#[cfg(not(windows))]
use crate::mirror::{NETWORK_DIR, write_network_files};
#[cfg(any(windows, test))]
use crate::options::LaunchOptions;
use crate::options::{DEFAULT_PREFIX_DIR, ProtonSwitch};
use crate::server::api_base_url;
use crate::settings::Settings;
use crate::steam::{self, Found};
use crate::theme::{self, LABEL_STYLE, LINE_WIDTH, Palette, TITLE_STYLE, ThemeChoice, mix};

/// Time between Discord login polls.
pub const DISCORD_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Longest wait for a Discord login.
pub const DISCORD_POLL_LIMIT: Duration = Duration::from_secs(300);
/// Repaint interval while a call runs.
const BUSY_REPAINT: Duration = Duration::from_millis(100);
/// Error code of an expired launcher session.
const NO_SESSION_CODE: &str = "no_session";

/// Time the state bar takes to grow after a change.
pub const BAR_GROWTH: Duration = Duration::from_millis(480);
/// Height of the state bar, of a section rule, and of the play button.
const BAR_HEIGHT: f32 = 3.0;
const RULE_HEIGHT: f32 = 2.0;
const PLAY_HEIGHT: f32 = 40.0;
/// Outer margin of the window content, and space around a section.
const WINDOW_MARGIN: i8 = 24;
const SECTION_GAP: f32 = 16.0;
/// Extra letter spacing of label type and of the state word.
const LABEL_TRACKING: f32 = 0.9;
/// Width of the fact label column, and of the server field.
const FACT_LABEL_WIDTH: f32 = 96.0;
const CONNECT_BUTTON_WIDTH: f32 = 88.0;
/// Width of the state word after a path field.
const PATH_STATE_WIDTH: f32 = 64.0;
/// Rows of the extra environment field.
const ENVIRONMENT_ROWS: usize = 3;
/// Whether the Proton options apply: the game runs under Proton on every system but Windows.
const USES_PROTON: bool = cfg!(not(windows));

/// Kind of a state, which picks its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Nothing runs and nothing is connected.
    Idle,
    /// A call runs.
    Busy,
    /// Connected, logged in, or the game started.
    Ready,
    /// The last call failed.
    Fault,
}

impl Tone {
    fn color(self, palette: &Palette) -> Color32 {
        match self {
            Self::Idle => palette.mute,
            Self::Busy => palette.warn,
            Self::Ready => palette.ready,
            Self::Fault => palette.alert,
        }
    }

    fn bar_color(self, palette: &Palette) -> Color32 {
        match self {
            Self::Idle => palette.line,
            Self::Busy => theme::SIGNAL_YELLOW,
            other => other.color(palette),
        }
    }
}

/// The status line: a state word and a detail line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Colour of the bar and the word.
    pub tone: Tone,
    /// The state word, lower case; the window shows it in capitals.
    pub word: &'static str,
    /// What happened, in one sentence.
    pub detail: String,
}

impl Status {
    fn new(tone: Tone, word: &'static str, detail: impl Into<String>) -> Self {
        Self {
            tone,
            word,
            detail: detail.into(),
        }
    }

    fn not_connected() -> Self {
        Self::new(
            Tone::Idle,
            "not connected",
            "Enter the server domain, for example example.org.",
        )
    }

    fn error(detail: impl Into<String>) -> Self {
        Self::new(Tone::Fault, "error", detail)
    }
}

/// What a path option must point at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathCheck {
    /// A file needed to play.
    RequiredFile,
    /// A folder needed to play.
    RequiredFolder,
    /// A file that may stay empty.
    OptionalFile,
    /// The Proton prefix: empty gives the default, a new folder is made by Proton, and Steam's
    /// retail prefix is refused.
    Prefix,
}

/// The state shown after a path field: its tone and word.
pub fn path_state(value: &str, check: PathCheck) -> (Tone, &'static str) {
    let path = Path::new(value);
    match check {
        PathCheck::Prefix if value.is_empty() => (Tone::Idle, "default"),
        PathCheck::Prefix if is_retail_prefix(path) => (Tone::Fault, "retail"),
        PathCheck::Prefix if path.is_dir() => (Tone::Ready, "found"),
        PathCheck::Prefix => (Tone::Idle, "new"),
        PathCheck::OptionalFile if value.is_empty() => (Tone::Idle, "not set"),
        _ if value.is_empty() => (Tone::Fault, "not set"),
        PathCheck::RequiredFolder if path.is_dir() => (Tone::Ready, "found"),
        PathCheck::RequiredFile | PathCheck::OptionalFile if path.is_file() => {
            (Tone::Ready, "found")
        }
        _ => (Tone::Fault, "missing"),
    }
}

/// Result of a worker job.
enum Event {
    Connected(String, LauncherApi, LauncherInfo),
    LoggedIn(String),
    GameStarted,
    LoggedOut,
    SessionExpired,
    Failed(String),
}

/// The launcher window state.
pub struct LauncherApp {
    settings_path: PathBuf,
    settings: Settings,
    server_input: String,
    username: String,
    password: String,
    connection: Option<(LauncherApi, LauncherInfo)>,
    status: Status,
    status_since: Instant,
    job: Option<Receiver<Event>>,
    /// What the last scan of this PC found, for the Proton pick list.
    found: Found,
    /// The loopback proxy that carries the game's AWS calls to the server. It must outlive the
    /// game, so the window keeps it; dropping it stops the proxy.
    proxy: Option<AwsProxy>,
}

fn run_job(job: impl FnOnce() -> Event + Send + 'static) -> Receiver<Event> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        // The window may have closed; then nobody reads the result.
        let _ = sender.send(job());
    });
    receiver
}

fn failure(error: &ApiError) -> Event {
    match error {
        ApiError::Server { code, .. } if code == NO_SESSION_CODE => Event::SessionExpired,
        other => Event::Failed(other.to_string()),
    }
}

/// The worker job that gets a game session key and starts the patched game directly on Windows (the
/// loopback proxy is started first, by `play`). Compiled on Linux too, so tests type-check it, but
/// called only on Windows.
#[cfg(any(windows, test))]
fn windows_play_job(
    api: LauncherApi,
    info: LauncherInfo,
    token: String,
    options: LaunchOptions,
) -> impl FnOnce() -> Event + Send + 'static {
    move || {
        let launch = match checked_windows_launch(options.windows_launch()) {
            Ok(launch) => launch,
            Err(error) => return Event::Failed(error.to_string()),
        };
        let environment = match options.environment(false) {
            Ok(environment) => environment,
            Err(error) => return Event::Failed(error.to_string()),
        };
        let key = match api.play(&token) {
            Ok(reply) => reply.game_session_key,
            Err(error) => return failure(&error),
        };
        match game_arguments(&info.launch_arguments, &key).and_then(|arguments| {
            start_game_windows(
                &launch,
                &append_override_config(arguments, options.override_config()),
                &environment,
            )
        }) {
            Ok(()) => Event::GameStarted,
            Err(error) => Event::Failed(error.to_string()),
        }
    }
}

fn wait_for_discord(api: &LauncherApi, state: &str) -> Event {
    let started = Instant::now();
    while started.elapsed() < DISCORD_POLL_LIMIT {
        match api.discord_poll(state) {
            Ok(DiscordPollReply::Ready(session)) => return Event::LoggedIn(session.launcher_token),
            Ok(DiscordPollReply::Pending) => std::thread::sleep(DISCORD_POLL_INTERVAL),
            Err(error) => return failure(&error),
        }
    }
    Event::Failed("Discord login timed out".to_owned())
}

/// The share of the state bar drawn `elapsed` after a change: a cubic ease-out over
/// [`BAR_GROWTH`], then the full bar.
pub fn bar_progress(elapsed: Duration) -> f32 {
    let linear = (elapsed.as_secs_f32() / BAR_GROWTH.as_secs_f32()).min(1.0);
    1.0 - (1.0 - linear).powi(3)
}

/// The name of a login mode.
pub fn auth_mode_name(mode: AuthMode) -> &'static str {
    match mode {
        AuthMode::Password => "Username and password",
        AuthMode::Discord => "Discord",
    }
}

/// Label type: condensed capitals in the mute colour.
fn label_text(text: &str, color: Color32) -> RichText {
    RichText::new(text.to_uppercase())
        .text_style(TextStyle::Name(LABEL_STYLE.into()))
        .extra_letter_spacing(LABEL_TRACKING)
        .color(color)
}

/// A full-width filled rule of `height`.
fn rule(ui: &mut egui::Ui, height: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, color);
}

/// A section: space, its head in label type, the content, then a 1px line.
fn section(ui: &mut egui::Ui, palette: &Palette, head: &str, content: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(SECTION_GAP);
    ui.label(label_text(head, palette.mute));
    content(ui);
    ui.add_space(SECTION_GAP);
    rule(ui, LINE_WIDTH, palette.line);
}

/// One fact row: a label column, then the value.
fn fact(ui: &mut egui::Ui, palette: &Palette, label: &str, value: RichText) {
    ui.horizontal(|ui| {
        ui.add_sized(
            vec2(FACT_LABEL_WIDTH, ui.spacing().interact_size.y),
            egui::Label::new(label_text(label, palette.mute)),
        );
        ui.label(value);
    });
}

/// A button filled with the accent colour, for the main action of a section.
fn primary_button(ui: &mut egui::Ui, palette: &Palette, text: &str, size: egui::Vec2) -> bool {
    ui.scope(|ui| {
        let widgets = &mut ui.visuals_mut().widgets;
        widgets.inactive.weak_bg_fill = palette.accent;
        widgets.hovered.weak_bg_fill = mix(palette.accent, palette.ground, theme::HOVER_MIX);
        for state in [&mut widgets.inactive, &mut widgets.hovered] {
            state.fg_stroke.color = palette.ground;
        }
        ui.add_sized(size, egui::Button::new(text)).clicked()
    })
    .inner
}

/// A path option row: the label column, the field, then its state word. True when the field lost
/// focus, so the options are saved.
fn path_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    label: &str,
    value: &mut String,
    hint: &str,
    check: PathCheck,
) -> bool {
    ui.horizontal(|ui| {
        ui.add_sized(
            vec2(FACT_LABEL_WIDTH, ui.spacing().interact_size.y),
            egui::Label::new(label_text(label, palette.mute)),
        );
        let field_width = ui.available_width() - PATH_STATE_WIDTH - ui.spacing().item_spacing.x;
        let field = ui.add_sized(
            vec2(field_width, ui.spacing().interact_size.y),
            egui::TextEdit::singleline(value).hint_text(hint),
        );
        let (tone, word) = path_state(value, check);
        ui.label(label_text(word, tone.color(palette)));
        field.lost_focus()
    })
    .inner
}

/// Whether Enter was pressed in the field of `response`.
fn submitted(ui: &egui::Ui, response: &egui::Response) -> bool {
    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
}

impl LauncherApp {
    /// The window state from saved settings; connects at once when a server is saved.
    pub fn new(settings_path: PathBuf, settings: Settings, found: Found) -> Self {
        let mut app = Self {
            server_input: settings.server.clone(),
            settings_path,
            settings,
            username: String::new(),
            password: String::new(),
            connection: None,
            status: Status::not_connected(),
            status_since: Instant::now(),
            job: None,
            found,
            proxy: None,
        };
        if !app.server_input.is_empty() {
            app.connect();
        }
        app
    }

    /// The status line now.
    pub fn status(&self) -> &Status {
        &self.status
    }

    fn set_status(&mut self, status: Status) {
        if status != self.status {
            self.status = status;
            self.status_since = Instant::now();
        }
    }

    fn save(&mut self) {
        if let Err(error) = self.settings.save(&self.settings_path) {
            self.set_status(Status::error(error.to_string()));
        }
    }

    /// The folder of the settings file, which also holds the default prefix.
    fn settings_dir(&self) -> PathBuf {
        self.settings_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    fn connect(&mut self) {
        let base = match api_base_url(&self.server_input) {
            Ok(base) => base,
            Err(error) => {
                self.set_status(Status::error(error.to_string()));
                return;
            }
        };
        self.set_status(Status::new(
            Tone::Busy,
            "connecting",
            format!("Connecting to {base}."),
        ));
        self.connection = None;
        let server = self.server_input.clone();
        self.job = Some(run_job(move || {
            match LauncherApi::new(base).and_then(|api| api.info().map(|info| (api, info))) {
                Ok((api, info)) => Event::Connected(server, api, info),
                Err(error) => Event::Failed(error.to_string()),
            }
        }));
    }

    fn password_login(&mut self, api: LauncherApi, register: bool) {
        let credentials = PasswordCredentials {
            username: self.username.clone(),
            password: std::mem::take(&mut self.password),
        };
        let detail = if register {
            "Making the account."
        } else {
            "Checking the username and password."
        };
        self.set_status(Status::new(Tone::Busy, "logging in", detail));
        self.job = Some(run_job(move || {
            let result = if register {
                api.register(&credentials)
            } else {
                api.login(&credentials)
            };
            match result {
                Ok(session) => Event::LoggedIn(session.launcher_token),
                Err(error) => Event::Failed(error.to_string()),
            }
        }));
    }

    fn discord_login(&mut self, api: LauncherApi) {
        self.set_status(Status::new(
            Tone::Busy,
            "logging in",
            "Finish the login in your browser.",
        ));
        self.job = Some(run_job(move || {
            let start = match api.discord_start() {
                Ok(start) => start,
                Err(error) => return Event::Failed(error.to_string()),
            };
            let page = match discord_login_page(&start.authorize_url) {
                Ok(page) => page,
                Err(error) => return Event::Failed(error.to_string()),
            };
            if let Err(error) = open::that(page.as_str()) {
                return Event::Failed(format!("cannot open the browser: {error}"));
            }
            wait_for_discord(&api, &start.state)
        }));
    }

    /// Starts the loopback proxy, checks the direct launch and the server's mirror, writes the
    /// private network files next to the settings (Linux), then asks for a game session key and
    /// starts the game.
    fn play(&mut self, api: LauncherApi, info: LauncherInfo, token: String) {
        self.set_status(Status::new(
            Tone::Busy,
            "starting",
            "Getting a game session and starting the game.",
        ));
        // Start the loopback proxy before the game and keep it for the window's life, so the
        // game's AWS calls reach the server for the whole session. A new play replaces it.
        self.proxy = None;
        match AwsProxy::start(&info.aws_mirror, api.host_name(), api.port()) {
            Ok(proxy) => self.proxy = Some(proxy),
            Err(error) => {
                self.set_status(Status::error(error.to_string()));
                return;
            }
        }
        let options = self.settings.options.clone();
        #[cfg(not(windows))]
        {
            let network_dir = self.settings_path.with_file_name(NETWORK_DIR);
            let settings_dir = self.settings_dir();
            self.job = Some(run_job(move || {
                let launch = match checked_direct_launch(options.direct_launch(&settings_dir)) {
                    Ok(launch) => launch,
                    Err(error) => return Event::Failed(error.to_string()),
                };
                let environment = match options.environment(true) {
                    Ok(environment) => environment,
                    Err(error) => return Event::Failed(error.to_string()),
                };
                let files =
                    match write_network_files(&network_dir, &info.aws_mirror, api.host_name()) {
                        Ok(files) => files,
                        Err(error) => return Event::Failed(error.to_string()),
                    };
                let key = match api.play(&token) {
                    Ok(reply) => reply.game_session_key,
                    Err(error) => return failure(&error),
                };
                match game_arguments(&info.launch_arguments, &key).and_then(|arguments| {
                    start_game(
                        &launch,
                        &files,
                        &append_override_config(arguments, options.override_config()),
                        &environment,
                    )
                }) {
                    Ok(()) => Event::GameStarted,
                    Err(error) => Event::Failed(error.to_string()),
                }
            }));
        }
        #[cfg(windows)]
        {
            self.job = Some(run_job(windows_play_job(api, info, token, options)));
        }
    }

    fn logout(&mut self, api: LauncherApi, token: String) {
        self.set_status(Status::new(
            Tone::Busy,
            "logging out",
            "Ending the session.",
        ));
        self.job = Some(run_job(move || match api.logout(&token) {
            Ok(()) => Event::LoggedOut,
            Err(error) => failure(&error),
        }));
    }

    fn set_theme(&mut self, ctx: &egui::Context, choice: ThemeChoice) {
        if self.settings.theme != choice {
            self.settings.theme = choice;
            theme::show(ctx, choice);
            self.save();
        }
    }

    fn apply(&mut self, event: Event) {
        match event {
            Event::Connected(server, api, info) => {
                self.set_status(Status::new(
                    Tone::Ready,
                    "connected",
                    format!("Connected to {}.", info.server_name),
                ));
                self.connection = Some((api, info));
                self.settings.set_server(&server);
                self.save();
            }
            Event::LoggedIn(token) => {
                self.set_status(Status::new(
                    Tone::Ready,
                    "logged in",
                    "Press Play to start the game.",
                ));
                self.settings.launcher_token = Some(token);
                self.save();
            }
            Event::GameStarted => self.set_status(Status::new(
                Tone::Ready,
                "game started",
                "The game process started.",
            )),
            Event::LoggedOut => {
                self.set_status(Status::new(Tone::Idle, "logged out", "Log in to play."));
                self.settings.launcher_token = None;
                self.save();
            }
            Event::SessionExpired => {
                self.set_status(Status::new(
                    Tone::Fault,
                    "login expired",
                    "Your login expired. Log in again.",
                ));
                self.settings.launcher_token = None;
                self.save();
            }
            Event::Failed(message) => self.set_status(Status::error(message)),
        }
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(receiver) = &self.job else {
            return;
        };
        match receiver.try_recv() {
            Ok(event) => {
                self.job = None;
                self.apply(event);
            }
            Err(TryRecvError::Empty) => ctx.request_repaint_after(BUSY_REPAINT),
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.set_status(Status::error("The call stopped."));
            }
        }
    }

    /// The masthead and the status line: the state bar, the state word and the detail.
    fn draw_status(&self, ui: &mut egui::Ui, palette: &Palette) {
        ui.label(
            RichText::new("Blighted New World")
                .heading()
                .color(palette.ink),
        );
        ui.add_space(SECTION_GAP);
        let progress = bar_progress(self.status_since.elapsed());
        if progress < 1.0 {
            ui.ctx().request_repaint();
        }
        let (track, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), BAR_HEIGHT), Sense::hover());
        let mut bar = track;
        bar.set_width(track.width() * progress);
        ui.painter()
            .rect_filled(bar, 0.0, self.status.tone.bar_color(palette));
        ui.label(
            RichText::new(self.status.word.to_uppercase())
                .text_style(TextStyle::Name(TITLE_STYLE.into()))
                .extra_letter_spacing(LABEL_TRACKING)
                .color(self.status.tone.color(palette)),
        );
        ui.label(RichText::new(&self.status.detail).color(palette.mute));
        ui.add_space(SECTION_GAP);
        rule(ui, RULE_HEIGHT, palette.ink);
    }

    fn draw_server(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        section(ui, palette, "Server", |ui| {
            ui.horizontal(|ui| {
                let field_width =
                    ui.available_width() - CONNECT_BUTTON_WIDTH - ui.spacing().item_spacing.x;
                let field = ui.add_sized(
                    vec2(field_width, ui.spacing().interact_size.y),
                    egui::TextEdit::singleline(&mut self.server_input).hint_text("example.org"),
                );
                let pressed = ui
                    .add_sized(
                        vec2(CONNECT_BUTTON_WIDTH, ui.spacing().interact_size.y),
                        egui::Button::new("Connect"),
                    )
                    .clicked();
                if pressed || submitted(ui, &field) {
                    self.connect();
                }
            });
            let Some((api, info)) = &self.connection else {
                return;
            };
            ui.add_space(SECTION_GAP / 2.0);
            fact(
                ui,
                palette,
                "Name",
                RichText::new(&info.server_name)
                    .text_style(TextStyle::Name(TITLE_STYLE.into()))
                    .color(palette.ink),
            );
            fact(
                ui,
                palette,
                "Address",
                RichText::new(api.host()).monospace().color(palette.ink),
            );
            fact(
                ui,
                palette,
                "Login",
                RichText::new(auth_mode_name(info.auth_mode)).color(palette.ink),
            );
            if info.auth_mode == AuthMode::Password {
                let sign_up = if info.allow_registration {
                    "Open"
                } else {
                    "Closed"
                };
                fact(
                    ui,
                    palette,
                    "Sign-up",
                    RichText::new(sign_up).color(palette.ink),
                );
            }
        });
    }

    fn draw_account(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        section(ui, palette, "Account", |ui| {
            let Some((api, info)) = self.connection.clone() else {
                ui.label(RichText::new("Connect to a server first.").color(palette.mute));
                return;
            };
            if let Some(token) = self.settings.launcher_token.clone() {
                let play_size = vec2(ui.available_width(), PLAY_HEIGHT);
                if primary_button(ui, palette, "Play", play_size) {
                    self.play(api.clone(), info, token.clone());
                }
                if ui.button("Log out").clicked() {
                    self.logout(api, token);
                }
                return;
            }
            match info.auth_mode {
                AuthMode::Password => self.draw_password_login(ui, palette, api, &info),
                AuthMode::Discord => {
                    ui.label(
                        RichText::new("Your browser opens Discord's login page.")
                            .color(palette.mute),
                    );
                    let size = vec2(ui.available_width(), ui.spacing().interact_size.y);
                    if primary_button(ui, palette, "Log in with Discord", size) {
                        self.discord_login(api);
                    }
                }
            }
        });
    }

    fn draw_password_login(
        &mut self,
        ui: &mut egui::Ui,
        palette: &Palette,
        api: LauncherApi,
        info: &LauncherInfo,
    ) {
        let field_size = |ui: &egui::Ui| vec2(ui.available_width(), ui.spacing().interact_size.y);
        let mut submit = false;
        for (label, secret) in [("Username", false), ("Password", true)] {
            ui.horizontal(|ui| {
                ui.add_sized(
                    vec2(FACT_LABEL_WIDTH, ui.spacing().interact_size.y),
                    egui::Label::new(label_text(label, palette.mute)),
                );
                let value = if secret {
                    &mut self.password
                } else {
                    &mut self.username
                };
                let field = ui.add_sized(
                    field_size(ui),
                    egui::TextEdit::singleline(value).password(secret),
                );
                submit |= secret && submitted(ui, &field);
            });
        }
        ui.horizontal(|ui| {
            ui.add_space(FACT_LABEL_WIDTH + ui.spacing().item_spacing.x);
            let size = vec2(CONNECT_BUTTON_WIDTH, ui.spacing().interact_size.y);
            let log_in = primary_button(ui, palette, "Log in", size) || submit;
            let register = info.allow_registration && ui.button("Make account").clicked();
            if log_in {
                self.password_login(api, false);
            } else if register {
                self.password_login(api, true);
            }
        });
    }

    /// Scans this PC again and fills the empty game and Steam paths.
    fn find_paths(&mut self) {
        self.found = steam::scan(|name| std::env::var(name).ok());
        self.settings.options.fill_from_scan(&self.found);
    }

    /// The Proton pick list of the builds found; true when the player picked one.
    fn draw_proton_list(&mut self, ui: &mut egui::Ui, palette: &Palette) -> bool {
        if self.found.protons.is_empty() {
            return false;
        }
        let proton = &mut self.settings.options.proton;
        let picked_name = self
            .found
            .protons
            .iter()
            .find(|build| build.program == Path::new(proton.as_str()))
            .map_or("Pick a Proton build", |build| build.name.as_str());
        let mut picked = false;
        ui.horizontal(|ui| {
            ui.add_space(FACT_LABEL_WIDTH + ui.spacing().item_spacing.x);
            egui::ComboBox::from_id_salt("proton build")
                .selected_text(RichText::new(picked_name).color(palette.ink))
                .show_ui(ui, |ui| {
                    for build in &self.found.protons {
                        let current = build.program == Path::new(proton.as_str());
                        if ui.selectable_label(current, &build.name).clicked() {
                            *proton = build.program.display().to_string();
                            picked = true;
                        }
                    }
                });
        });
        picked
    }

    fn draw_options(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let default_prefix = self
            .settings_dir()
            .join(DEFAULT_PREFIX_DIR)
            .display()
            .to_string();
        section(ui, palette, "Options", |ui| {
            let mut save = path_row(
                ui,
                palette,
                "Game",
                &mut self.settings.options.game_exe,
                PATCHED_EXE_NAME,
                PathCheck::RequiredFile,
            );
            if USES_PROTON {
                save |= path_row(
                    ui,
                    palette,
                    "Proton",
                    &mut self.settings.options.proton,
                    "proton",
                    PathCheck::RequiredFile,
                );
                save |= self.draw_proton_list(ui, palette);
                let options = &mut self.settings.options;
                save |= path_row(
                    ui,
                    palette,
                    "Prefix",
                    &mut options.prefix,
                    &default_prefix,
                    PathCheck::Prefix,
                );
                save |= path_row(
                    ui,
                    palette,
                    "Steam",
                    &mut options.steam_install,
                    "Steam folder",
                    PathCheck::RequiredFolder,
                );
            }
            let options = &mut self.settings.options;
            save |= path_row(
                ui,
                palette,
                "Override",
                &mut options.override_config,
                "ClientOverride.json (optional)",
                PathCheck::OptionalFile,
            );
            if USES_PROTON {
                ui.add_space(SECTION_GAP / 2.0);
                for switch in ProtonSwitch::ALL {
                    let mut on = options.proton_switches.contains(&switch);
                    if ui.checkbox(&mut on, switch.label()).changed() {
                        if on {
                            options.proton_switches.insert(switch);
                        } else {
                            options.proton_switches.remove(&switch);
                        }
                        save = true;
                    }
                }
            }
            ui.add_space(SECTION_GAP / 2.0);
            ui.label(label_text("Extra environment", palette.mute));
            ui.label(RichText::new("One NAME=value per line.").color(palette.mute));
            save |= ui
                .add(
                    egui::TextEdit::multiline(&mut options.environment)
                        .desired_rows(ENVIRONMENT_ROWS)
                        .desired_width(f32::INFINITY)
                        .code_editor(),
                )
                .lost_focus();
            ui.add_space(SECTION_GAP / 2.0);
            if ui.button("Find paths").clicked() {
                self.find_paths();
                save = true;
            }
            if save {
                self.save();
            }
        });
    }

    fn draw_settings(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let ctx = ui.ctx().clone();
        section(ui, palette, "Settings", |ui| {
            ui.horizontal(|ui| {
                ui.add_sized(
                    vec2(FACT_LABEL_WIDTH, ui.spacing().interact_size.y),
                    egui::Label::new(label_text("Theme", palette.mute)),
                );
                for choice in ThemeChoice::ALL {
                    let size = vec2(CONNECT_BUTTON_WIDTH, ui.spacing().interact_size.y);
                    let pressed = if self.settings.theme == choice {
                        primary_button(ui, palette, choice.label(), size)
                    } else {
                        ui.add_sized(size, egui::Button::new(choice.label()))
                            .clicked()
                    };
                    if pressed {
                        self.set_theme(&ctx, choice);
                    }
                }
            });
            fact(
                ui,
                palette,
                "Saved in",
                RichText::new(self.settings_path.display().to_string())
                    .monospace()
                    .color(palette.mute),
            );
        });
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        let palette = Palette::current(ui.visuals());
        self.draw_status(ui, palette);
        let busy = self.job.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            self.draw_server(ui, palette);
            self.draw_account(ui, palette);
        });
        self.draw_options(ui, palette);
        self.draw_settings(ui, palette);
    }
}

impl eframe::App for LauncherApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_job(ui.ctx());
        egui::Frame::central_panel(ui.style())
            .inner_margin(WINDOW_MARGIN)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.draw(ui));
            });
    }
}

#[cfg(test)]
mod tests {
    use nw_protocol::aws_mirror::{AwsMirror, CLIENT_PING_PORT};

    use super::*;

    fn app() -> (tempfile::TempDir, LauncherApp) {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("settings.json");
        let app = LauncherApp::new(path, Settings::default(), Found::default());
        (dir, app)
    }

    fn info() -> LauncherInfo {
        LauncherInfo {
            server_name: "Test world".to_owned(),
            auth_mode: AuthMode::Password,
            allow_registration: true,
            launch_arguments: Vec::new(),
            aws_mirror: AwsMirror {
                hosts: Vec::new(),
                certificate_pem: String::new(),
                ping_port: CLIENT_PING_PORT,
            },
        }
    }

    #[test]
    fn the_windows_play_job_is_a_sendable_worker() {
        // Building the job (without running it) type-checks the whole Windows start path on Linux;
        // `run_job` needs a `Send + 'static` closure.
        fn assert_send_static<T: Send + 'static>(_: &T) {}
        let api = LauncherApi::new("https://login.example.org".to_owned()).expect("api");
        let job = windows_play_job(api, info(), "token".to_owned(), LaunchOptions::default());
        assert_send_static(&job);
    }

    #[test]
    fn a_new_launcher_with_no_server_is_not_connected() {
        let (_dir, app) = app();
        assert_eq!(app.status(), &Status::not_connected());
        assert_eq!(app.status().tone, Tone::Idle);
    }

    #[test]
    fn a_bad_address_is_an_error_and_starts_no_call() {
        let (_dir, mut app) = app();
        app.server_input = "localhost".to_owned();
        app.connect();
        assert_eq!(app.status().tone, Tone::Fault);
        assert!(app.job.is_none());
    }

    #[test]
    fn events_set_the_state_word_and_the_saved_settings() {
        let (_dir, mut app) = app();
        let api = LauncherApi::new("https://login.example.org".to_owned()).expect("api");
        app.apply(Event::Connected("example.org".to_owned(), api, info()));
        assert_eq!(
            (app.status().tone, app.status().word),
            (Tone::Ready, "connected")
        );
        assert_eq!(app.status().detail, "Connected to Test world.");
        app.apply(Event::LoggedIn("token".to_owned()));
        assert_eq!(app.status().word, "logged in");
        let saved = Settings::load(&app.settings_path).expect("load");
        assert_eq!(saved.server, "example.org");
        assert_eq!(saved.launcher_token.as_deref(), Some("token"));
        app.apply(Event::SessionExpired);
        assert_eq!(
            (app.status().tone, app.status().word),
            (Tone::Fault, "login expired")
        );
        assert_eq!(app.settings.launcher_token, None);
        app.apply(Event::Failed("server unreachable".to_owned()));
        assert_eq!(app.status(), &Status::error("server unreachable"));
    }

    #[test]
    fn a_new_state_restarts_the_bar_and_the_same_state_does_not() {
        let (_dir, mut app) = app();
        let first = app.status_since;
        app.set_status(Status::not_connected());
        assert_eq!(app.status_since, first);
        app.set_status(Status::error("x"));
        assert!(app.status_since >= first);
        assert_eq!(app.status().word, "error");
    }

    #[test]
    fn the_bar_grows_with_an_ease_out_and_then_stays_full() {
        assert_eq!(bar_progress(Duration::ZERO), 0.0);
        let half = bar_progress(BAR_GROWTH / 2);
        assert!(half > 0.5 && half < 1.0, "{half}");
        assert_eq!(bar_progress(BAR_GROWTH), 1.0);
        assert_eq!(bar_progress(BAR_GROWTH * 2), 1.0);
    }

    #[test]
    fn tones_use_the_palette_state_colours() {
        let palette = &theme::NIGHT;
        assert_eq!(Tone::Ready.color(palette), palette.ready);
        assert_eq!(Tone::Fault.bar_color(palette), palette.alert);
        assert_eq!(Tone::Busy.bar_color(palette), theme::SIGNAL_YELLOW);
        assert_eq!(Tone::Idle.bar_color(palette), palette.line);
    }

    #[test]
    fn path_states_name_what_is_wrong() {
        let dir = tempfile::tempdir().expect("dir");
        let folder = dir.path().display().to_string();
        let file = dir.path().join("BlightedNewWorld.exe");
        std::fs::write(&file, "").expect("file");
        let file = file.display().to_string();
        let missing = dir.path().join("none").display().to_string();
        let cases = [
            ("", PathCheck::RequiredFile, (Tone::Fault, "not set")),
            ("", PathCheck::OptionalFile, (Tone::Idle, "not set")),
            ("", PathCheck::Prefix, (Tone::Idle, "default")),
            (
                file.as_str(),
                PathCheck::RequiredFile,
                (Tone::Ready, "found"),
            ),
            (
                file.as_str(),
                PathCheck::RequiredFolder,
                (Tone::Fault, "missing"),
            ),
            (
                folder.as_str(),
                PathCheck::RequiredFolder,
                (Tone::Ready, "found"),
            ),
            (
                missing.as_str(),
                PathCheck::OptionalFile,
                (Tone::Fault, "missing"),
            ),
            (folder.as_str(), PathCheck::Prefix, (Tone::Ready, "found")),
            (missing.as_str(), PathCheck::Prefix, (Tone::Idle, "new")),
            (
                "/x/steamapps/compatdata/1063730",
                PathCheck::Prefix,
                (Tone::Fault, "retail"),
            ),
        ];
        for (value, check, state) in cases {
            assert_eq!(path_state(value, check), state, "{value:?} {check:?}");
        }
    }

    #[test]
    fn login_modes_have_plain_names() {
        assert_eq!(auth_mode_name(AuthMode::Discord), "Discord");
        assert_eq!(auth_mode_name(AuthMode::Password), "Username and password");
    }
}
