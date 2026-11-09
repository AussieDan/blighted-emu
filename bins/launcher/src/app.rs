// SPDX-License-Identifier: GPL-3.0-or-later
//! The launcher window (M2 spec section 6), the same on Windows and Linux, in the look and flow
//! of the project website. Network calls run on worker threads; the window reads their results
//! each frame. A top bar holds the wordmark and the page links (Home, Options, Settings). Home
//! shows the server and the way to play first: the server name, its state line (a state dot, a
//! word and a detail), its facts, Play or the login, then the saved servers; the server news
//! ([`crate::news_view`]) sits beside it in a wide window and below it in a narrow one.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, Rect, RichText, Sense, Stroke, StrokeKind,
    TextStyle, UiBuilder, pos2, vec2,
};
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
use crate::news_view::NewsPanel;
#[cfg(any(windows, test))]
use crate::options::LaunchOptions;
use crate::options::{DEFAULT_PREFIX_DIR, ProtonSwitch};
use crate::server::api_base_url;
use crate::settings::{ServerProfile, Settings, same_server};
use crate::steam::{self, Found};
use crate::theme::{
    self, CORNER, LABEL_STYLE, LINE_WIDTH, Palette, TITLE_STYLE, ThemeChoice, WORDMARK_STYLE,
};

/// Time between Discord login polls.
pub const DISCORD_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Longest wait for a Discord login.
pub const DISCORD_POLL_LIMIT: Duration = Duration::from_secs(300);
/// Repaint interval while a call runs.
const BUSY_REPAINT: Duration = Duration::from_millis(100);
/// Error code of an expired launcher session.
const NO_SESSION_CODE: &str = "no_session";

/// The wordmark in the top bar.
const WORDMARK: &str = "Blighted New World";
/// Space left and right of the top bar and the page, and above and below the top bar text.
const WINDOW_MARGIN: i8 = 24;
const TOP_BAR_PADDING: i8 = 14;
/// Space between the page links of the top bar.
const NAV_GAP: f32 = 26.0;
/// A window at least this wide shows Home in two columns: the play column at its width, the
/// gap, then the news.
pub const TWO_COLUMN_MIN_WIDTH: f32 = 760.0;
const PLAY_COLUMN_WIDTH: f32 = 360.0;
const COLUMN_GAP: f32 = 40.0;
/// Widest text column of the Options and Settings pages.
const PAGE_MAX_WIDTH: f32 = 640.0;
/// Scroll ids of the Home columns.
const PLAY_SCROLL: &str = "play column";
const NEWS_SCROLL: &str = "news column";
/// Height of the play button.
const PLAY_HEIGHT: f32 = 44.0;
/// Space between sections, and between a fact label and its value.
const SECTION_GAP: f32 = 22.0;
const FACT_GAP: f32 = 18.0;
/// Width of the fact label column.
const FACT_LABEL_WIDTH: f32 = 72.0;
/// Width of the connect and remove buttons, and of the theme choices.
const CONNECT_BUTTON_WIDTH: f32 = 96.0;
/// Space inside a picker row, left and right of its text.
const ROW_PADDING: f32 = 12.0;
/// Width of the state word after a path field.
const PATH_STATE_WIDTH: f32 = 64.0;
/// Radius of the state dot, and the ring width of the idle dot (the website's "no signal").
const DOT_RADIUS: f32 = 5.0;
const IDLE_RING_WIDTH: f32 = 2.0;
/// Rows of the extra environment field.
const ENVIRONMENT_ROWS: usize = 3;
/// The Home heading when no server is connected.
const NO_SERVER_HEADING: &str = "No server";
/// Whether the Proton options apply: the game runs under Proton on every system but Windows.
const USES_PROTON: bool = cfg!(not(windows));

/// A page of the window, picked in the top bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// Server, login, play and news.
    Home,
    /// How the game starts.
    Options,
    /// Theme and the settings file.
    Settings,
}

impl Page {
    /// The pages in top bar order.
    pub const ALL: [Self; 3] = [Self::Home, Self::Options, Self::Settings];

    /// The link text, and the heading of the page.
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Options => "Options",
            Self::Settings => "Settings",
        }
    }
}

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
            Self::Idle => palette.ink_2,
            Self::Busy => palette.warn,
            Self::Ready => palette.good,
            Self::Fault => palette.bad,
        }
    }
}

/// The status line: a state word and a detail line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Colour of the dot.
    pub tone: Tone,
    /// The state word, lower case; the window starts it with a capital.
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
            "Pick a saved server, or enter the server domain, for example example.org.",
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
    /// The name field of the selected saved server.
    name_input: String,
    username: String,
    password: String,
    connection: Option<(LauncherApi, LauncherInfo)>,
    status: Status,
    job: Option<Receiver<Event>>,
    page: Page,
    news: NewsPanel,
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

/// The name of a login mode.
pub fn auth_mode_name(mode: AuthMode) -> &'static str {
    match mode {
        AuthMode::Password => "Username and password",
        AuthMode::Discord => "Discord",
    }
}

/// The state word with a capital first letter.
fn capitalized(word: &str) -> String {
    let mut characters = word.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

/// A field label: the label style in ink, above its field.
fn field_label(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.label(
        RichText::new(text)
            .text_style(TextStyle::Name(LABEL_STYLE.into()))
            .color(palette.ink),
    );
}

/// A one-line text field with its text in the middle of the control height.
fn text_field(value: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(value).vertical_align(Align::Center)
}

/// A section heading in the title style.
fn title(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.label(
        RichText::new(text)
            .text_style(TextStyle::Name(TITLE_STYLE.into()))
            .color(palette.ink),
    );
}

/// A full-width filled line of `height`.
fn rule(ui: &mut egui::Ui, height: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, color);
}

/// A rule with space above and below, between the parts of a page.
fn divider(ui: &mut egui::Ui, palette: &Palette) {
    ui.add_space(SECTION_GAP);
    rule(ui, LINE_WIDTH, palette.rule);
    ui.add_space(SECTION_GAP);
}

/// One fact row: the label column in the secondary ink, then the value.
fn fact(ui: &mut egui::Ui, palette: &Palette, label: &str, value: RichText) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = FACT_GAP;
        ui.add_sized(
            vec2(FACT_LABEL_WIDTH, ui.text_style_height(&TextStyle::Body)),
            egui::Label::new(RichText::new(label).color(palette.ink_2)),
        );
        ui.label(value);
    });
}

/// The state dot of `tone`: filled, or a ring when idle.
fn state_dot(ui: &mut egui::Ui, palette: &Palette, tone: Tone) {
    let side = DOT_RADIUS * 2.0;
    let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    let color = tone.color(palette);
    if tone == Tone::Idle {
        ui.painter().circle_stroke(
            rect.center(),
            DOT_RADIUS - IDLE_RING_WIDTH / 2.0,
            Stroke::new(IDLE_RING_WIDTH, color),
        );
    } else {
        ui.painter().circle_filled(rect.center(), DOT_RADIUS, color);
    }
}

/// The state line: the dot and the word, then the detail in the secondary ink.
fn status_line(ui: &mut egui::Ui, palette: &Palette, status: &Status) {
    ui.horizontal(|ui| {
        state_dot(ui, palette, status.tone);
        ui.label(RichText::new(capitalized(status.word)).color(palette.ink));
    });
    ui.label(RichText::new(&status.detail).color(palette.ink_2));
}

/// A button filled with violet, for the one main action of a page.
fn primary_button(ui: &mut egui::Ui, palette: &Palette, text: &str, size: egui::Vec2) -> bool {
    ui.scope(|ui| {
        let widgets = &mut ui.visuals_mut().widgets;
        for (state, fill) in [
            (&mut widgets.inactive, palette.violet),
            (&mut widgets.hovered, palette.violet_hover),
            (&mut widgets.active, palette.violet_hover),
        ] {
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(LINE_WIDTH, fill);
            state.fg_stroke.color = palette.on_violet;
        }
        ui.add_sized(size, egui::Button::new(text)).clicked()
    })
    .inner
}

/// A path option: its label above, then the field and its state word. True when the field lost
/// focus, so the options are saved.
fn path_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    label: &str,
    value: &mut String,
    hint: &str,
    check: PathCheck,
) -> bool {
    field_label(ui, palette, label);
    ui.horizontal(|ui| {
        let field_width = ui.available_width() - PATH_STATE_WIDTH - ui.spacing().item_spacing.x;
        let field = ui.add_sized(
            vec2(field_width, ui.spacing().interact_size.y),
            text_field(value).hint_text(hint),
        );
        let (tone, word) = path_state(value, check);
        ui.label(
            RichText::new(word)
                .text_style(TextStyle::Name(LABEL_STYLE.into()))
                .color(tone.color(palette)),
        );
        field.lost_focus()
    })
    .inner
}

/// The fill and outline of a picker choice (the website's picker links): the selected one is
/// raised and outlined in violet, a hovered one is outlined in violet, the others in vein; the
/// focused one gets the focus outline. Returns the text colour.
fn paint_picker(
    ui: &egui::Ui,
    palette: &Palette,
    rect: Rect,
    response: &egui::Response,
    selected: bool,
) -> Color32 {
    let corner = CornerRadius::same(CORNER);
    if selected {
        ui.painter().rect_filled(rect, corner, palette.stone_hi);
    }
    let outline = if response.has_focus() {
        ui.visuals().selection.stroke
    } else if selected || response.hovered() {
        Stroke::new(LINE_WIDTH, palette.violet)
    } else {
        Stroke::new(LINE_WIDTH, palette.vein)
    };
    ui.painter()
        .rect_stroke(rect, corner, outline, StrokeKind::Inside);
    if selected || response.hovered() {
        palette.ink
    } else {
        palette.ink_2
    }
}

/// A picker choice of `size` with one text; true when picked.
fn picker_button(
    ui: &mut egui::Ui,
    palette: &Palette,
    text: &str,
    size: egui::Vec2,
    selected: bool,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, selected, text)
    });
    let color = paint_picker(ui, palette, rect, &response, selected);
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), TextStyle::Body.resolve(ui.style()), color);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response.clicked()
}

/// One saved server row, as wide as the column: its name, then its address at the right end, in
/// the picker look. True when the player picked it (a click, or Enter or Space with the focus on
/// it).
fn profile_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    profile: &ServerProfile,
    selected: bool,
) -> bool {
    let size = vec2(ui.available_width(), ui.spacing().interact_size.y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            enabled,
            selected,
            &profile.name,
        )
    });
    let color = paint_picker(ui, palette, rect, &response, selected);
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(ROW_PADDING, 0.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    row.add(
        egui::Label::new(
            RichText::new(&profile.server)
                .monospace()
                .color(palette.ink_2),
        )
        .selectable(false),
    );
    row.with_layout(Layout::left_to_right(Align::Center), |row| {
        row.add(
            egui::Label::new(RichText::new(&profile.name).color(color))
                .selectable(false)
                .truncate(),
        );
    });
    response.clicked()
}

/// A page link of the top bar: ink when current, hovered or focused, else the secondary ink.
fn nav_link(ui: &mut egui::Ui, palette: &Palette, page: Page, current: bool) -> bool {
    let font = TextStyle::Body.resolve(ui.style());
    let size = ui
        .painter()
        .layout_no_wrap(page.label().to_owned(), font.clone(), palette.ink)
        .size();
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Link, enabled, current, page.label())
    });
    let color = if current || response.hovered() || response.has_focus() {
        palette.ink
    } else {
        palette.ink_2
    };
    let galley = ui
        .painter()
        .layout_no_wrap(page.label().to_owned(), font, color);
    ui.painter().galley(rect.min, galley, color);
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(DOT_RADIUS),
            CornerRadius::same(CORNER),
            ui.visuals().selection.stroke,
            StrokeKind::Outside,
        );
    }
    response.clicked()
}

/// The folder of a file path.
fn folder_of(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Whether Enter was pressed in the field of `response`.
fn submitted(ui: &egui::Ui, response: &egui::Response) -> bool {
    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))
}

impl LauncherApp {
    /// The window state from saved settings; connects at once when a server is saved.
    pub fn new(settings_path: PathBuf, settings: Settings, found: Found) -> Self {
        let selected = settings.selected_profile();
        let settings_dir = folder_of(&settings_path);
        let mut app = Self {
            server_input: selected
                .map(|profile| profile.server.clone())
                .unwrap_or_default(),
            name_input: selected
                .map(|profile| profile.name.clone())
                .unwrap_or_default(),
            settings_path,
            settings,
            username: String::new(),
            password: String::new(),
            connection: None,
            status: Status::not_connected(),
            job: None,
            page: Page::Home,
            news: NewsPanel::open(settings_dir),
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
        self.status = status;
    }

    fn save(&mut self) {
        if let Err(error) = self.settings.save(&self.settings_path) {
            self.set_status(Status::error(error.to_string()));
        }
    }

    /// The folder of the settings file, which also holds the default prefix and the news cache.
    fn settings_dir(&self) -> PathBuf {
        folder_of(&self.settings_path)
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

    /// Connects to the saved server `server`.
    fn pick(&mut self, server: &str) {
        self.server_input = server.to_owned();
        self.connect();
    }

    /// Saves the name field as the selected server's name; an empty field gets the old name back.
    fn rename_selected(&mut self) {
        let Some(server) = self.settings.selected.clone() else {
            return;
        };
        self.settings.rename(&server, &self.name_input);
        self.name_input = self
            .settings
            .selected_profile()
            .map(|profile| profile.name.clone())
            .unwrap_or_default();
        self.save();
    }

    /// Forgets the selected server and its login, and leaves it.
    fn remove_selected(&mut self) {
        let Some(server) = self.settings.selected.clone() else {
            return;
        };
        self.settings.remove(&server);
        self.connection = None;
        self.server_input.clear();
        self.name_input.clear();
        self.set_status(Status::not_connected());
        self.save();
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
                self.settings.connected(&server, &info.server_name);
                self.name_input = self
                    .settings
                    .selected_profile()
                    .map(|profile| profile.name.clone())
                    .unwrap_or_default();
                self.news.connected(&api);
                self.connection = Some((api, info));
                self.save();
            }
            Event::LoggedIn(token) => {
                self.set_status(Status::new(
                    Tone::Ready,
                    "logged in",
                    "Press Play to start the game.",
                ));
                self.settings.set_token(Some(token));
                self.save();
            }
            Event::GameStarted => self.set_status(Status::new(
                Tone::Ready,
                "game started",
                "The game process started.",
            )),
            Event::LoggedOut => {
                self.set_status(Status::new(Tone::Idle, "logged out", "Log in to play."));
                self.settings.set_token(None);
                self.save();
            }
            Event::SessionExpired => {
                self.set_status(Status::new(
                    Tone::Fault,
                    "login expired",
                    "Your login expired. Log in again.",
                ));
                self.settings.set_token(None);
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

    /// The page heading and the state line.
    fn draw_heading(&self, ui: &mut egui::Ui, palette: &Palette, heading: &str) {
        ui.label(RichText::new(heading).heading().color(palette.ink));
        status_line(ui, palette, &self.status);
        ui.add_space(SECTION_GAP);
    }

    /// The connected server's facts.
    fn draw_facts(ui: &mut egui::Ui, palette: &Palette, api: &LauncherApi, info: &LauncherInfo) {
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
        ui.add_space(SECTION_GAP);
    }

    /// The play column of Home: the server name and state, its facts, play or login, then the
    /// saved servers. Disabled while a call runs.
    fn draw_play_column(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let heading = self
            .connection
            .as_ref()
            .map_or(NO_SERVER_HEADING, |(_, info)| info.server_name.as_str())
            .to_owned();
        self.draw_heading(ui, palette, &heading);
        let busy = self.job.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            if let Some((api, info)) = &self.connection {
                Self::draw_facts(ui, palette, api, info);
            }
            self.draw_account(ui, palette);
            divider(ui, palette);
            self.draw_servers(ui, palette);
        });
    }

    fn draw_servers(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        title(ui, palette, "Servers");
        let mut picked = None;
        for profile in &self.settings.profiles {
            let selected = self
                .settings
                .selected
                .as_deref()
                .is_some_and(|server| same_server(server, &profile.server));
            if profile_row(ui, palette, profile, selected) {
                picked = Some(profile.server.clone());
            }
        }
        if let Some(server) = picked {
            self.pick(&server);
        }
        if !self.settings.profiles.is_empty() {
            ui.add_space(SECTION_GAP / 2.0);
        }
        field_label(ui, palette, "Server address");
        ui.horizontal(|ui| {
            let field_width =
                ui.available_width() - CONNECT_BUTTON_WIDTH - ui.spacing().item_spacing.x;
            let field = ui.add_sized(
                vec2(field_width, ui.spacing().interact_size.y),
                text_field(&mut self.server_input).hint_text("example.org"),
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
        let typed = self.server_input.trim();
        let is_new = !typed.is_empty()
            && !self
                .settings
                .profiles
                .iter()
                .any(|profile| same_server(&profile.server, typed));
        if is_new {
            ui.label(
                RichText::new("A new address is saved here once it connects.")
                    .small()
                    .color(palette.ink_2),
            );
        }
        if self.connection.is_some() && self.settings.selected_profile().is_some() {
            self.draw_saved_as(ui, palette);
        }
    }

    /// The selected server's name field, then the remove button.
    fn draw_saved_as(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add_space(SECTION_GAP / 2.0);
        field_label(ui, palette, "Saved as");
        let (renamed, remove) = ui
            .horizontal(|ui| {
                let field_width =
                    ui.available_width() - CONNECT_BUTTON_WIDTH - ui.spacing().item_spacing.x;
                let field = ui.add_sized(
                    vec2(field_width, ui.spacing().interact_size.y),
                    text_field(&mut self.name_input).hint_text("Server name"),
                );
                let remove = ui
                    .add_sized(
                        vec2(CONNECT_BUTTON_WIDTH, ui.spacing().interact_size.y),
                        egui::Button::new("Remove"),
                    )
                    .on_hover_text("Forget this server and its login.")
                    .clicked();
                (field.lost_focus(), remove)
            })
            .inner;
        if remove {
            self.remove_selected();
        } else if renamed {
            self.rename_selected();
        }
    }

    fn draw_account(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let Some((api, info)) = self.connection.clone() else {
            ui.label(RichText::new("Connect to a server first.").color(palette.ink_2));
            return;
        };
        if let Some(token) = self.settings.token().map(str::to_owned) {
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
                    RichText::new("Your browser opens Discord's login page.").color(palette.ink_2),
                );
                let size = vec2(ui.available_width(), PLAY_HEIGHT);
                if primary_button(ui, palette, "Log In with Discord", size) {
                    self.discord_login(api);
                }
            }
        }
    }

    fn draw_password_login(
        &mut self,
        ui: &mut egui::Ui,
        palette: &Palette,
        api: LauncherApi,
        info: &LauncherInfo,
    ) {
        let mut submit = false;
        for (label, secret) in [("Username", false), ("Password", true)] {
            field_label(ui, palette, label);
            let value = if secret {
                &mut self.password
            } else {
                &mut self.username
            };
            let field = ui.add_sized(
                vec2(ui.available_width(), ui.spacing().interact_size.y),
                text_field(value).password(secret),
            );
            submit |= secret && submitted(ui, &field);
        }
        ui.add_space(SECTION_GAP / 2.0);
        ui.horizontal(|ui| {
            let size = vec2(CONNECT_BUTTON_WIDTH, ui.spacing().interact_size.y);
            let log_in = primary_button(ui, palette, "Log In", size) || submit;
            let register = info.allow_registration && ui.button("Make Account").clicked();
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
        picked
    }

    fn draw_options(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        self.draw_heading(ui, palette, Page::Options.label());
        let default_prefix = self
            .settings_dir()
            .join(DEFAULT_PREFIX_DIR)
            .display()
            .to_string();
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
        field_label(ui, palette, "Extra environment");
        ui.label(
            RichText::new("One NAME=value per line.")
                .small()
                .color(palette.ink_2),
        );
        save |= ui
            .add(
                egui::TextEdit::multiline(&mut options.environment)
                    .desired_rows(ENVIRONMENT_ROWS)
                    .desired_width(f32::INFINITY)
                    .code_editor(),
            )
            .lost_focus();
        ui.add_space(SECTION_GAP / 2.0);
        if ui.button("Find Paths").clicked() {
            self.find_paths();
            save = true;
        }
        if save {
            self.save();
        }
    }

    fn draw_settings(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        self.draw_heading(ui, palette, Page::Settings.label());
        let ctx = ui.ctx().clone();
        field_label(ui, palette, "Theme");
        ui.horizontal(|ui| {
            for choice in ThemeChoice::ALL {
                let size = vec2(CONNECT_BUTTON_WIDTH, ui.spacing().interact_size.y);
                let selected = self.settings.theme == choice;
                if picker_button(ui, palette, choice.label(), size, selected) {
                    self.set_theme(&ctx, choice);
                }
            }
        });
        ui.add_space(SECTION_GAP / 2.0);
        field_label(ui, palette, "Saved in");
        ui.label(
            RichText::new(self.settings_path.display().to_string())
                .monospace()
                .color(palette.ink_2),
        );
    }

    /// The top bar: the wordmark, then the page links at the right end, above a rule.
    fn draw_top_bar(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        egui::Frame::new()
            .fill(palette.abyss)
            .inner_margin(Margin::symmetric(WINDOW_MARGIN, TOP_BAR_PADDING))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(WORDMARK)
                            .text_style(TextStyle::Name(WORDMARK_STYLE.into()))
                            .color(palette.ink),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = NAV_GAP;
                        for page in Page::ALL.into_iter().rev() {
                            if nav_link(ui, palette, page, self.page == page) {
                                self.page = page;
                            }
                        }
                    });
                });
            });
        rule(ui, LINE_WIDTH, palette.rule);
    }

    /// Home: the play column and the news side by side in a wide window, else one below the
    /// other. Returns the error of a news link that could not be opened.
    fn draw_home(&mut self, ui: &mut egui::Ui, palette: &Palette) -> Option<String> {
        if ui.available_width() < TWO_COLUMN_MIN_WIDTH {
            return egui::ScrollArea::vertical()
                .id_salt(PLAY_SCROLL)
                .auto_shrink(false)
                .show(ui, |ui| {
                    self.draw_play_column(ui, palette);
                    divider(ui, palette);
                    self.news.draw(ui, palette)
                })
                .inner;
        }
        let full = ui.available_rect_before_wrap();
        let play = Rect::from_min_size(full.min, vec2(PLAY_COLUMN_WIDTH, full.height()));
        let news = Rect::from_min_max(pos2(play.max.x + COLUMN_GAP, full.min.y), full.max);
        let column = |ui: &mut egui::Ui, rect: Rect| {
            ui.new_child(
                UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::top_down(Align::Min)),
            )
        };
        let mut play_ui = column(ui, play);
        egui::ScrollArea::vertical()
            .id_salt(PLAY_SCROLL)
            .auto_shrink(false)
            .show(&mut play_ui, |ui| self.draw_play_column(ui, palette));
        let mut news_ui = column(ui, news);
        let failure = egui::ScrollArea::vertical()
            .id_salt(NEWS_SCROLL)
            .auto_shrink(false)
            .show(&mut news_ui, |ui| self.news.draw(ui, palette))
            .inner;
        ui.advance_cursor_after_rect(full);
        failure
    }

    /// A text page (Options or Settings): `content` in one scrolling column.
    fn draw_text_page(
        &mut self,
        ui: &mut egui::Ui,
        palette: &Palette,
        page: Page,
        content: fn(&mut Self, &mut egui::Ui, &Palette),
    ) {
        egui::ScrollArea::vertical()
            .id_salt(page.label())
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.set_max_width(PAGE_MAX_WIDTH);
                content(self, ui, palette);
            });
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        let palette = Palette::current(ui.visuals());
        let spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.y = 0.0;
        self.draw_top_bar(ui, palette);
        egui::Frame::new()
            .inner_margin(Margin::same(WINDOW_MARGIN))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = spacing;
                match self.page {
                    Page::Home => {
                        if let Some(error) = self.draw_home(ui, palette) {
                            self.set_status(Status::error(error));
                        }
                    }
                    Page::Options => {
                        self.draw_text_page(ui, palette, Page::Options, Self::draw_options);
                    }
                    Page::Settings => {
                        self.draw_text_page(ui, palette, Page::Settings, Self::draw_settings);
                    }
                }
            });
    }
}

impl eframe::App for LauncherApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_job(ui.ctx());
        let connection = self.connection.as_ref().map(|(api, _)| api);
        self.news.update(ui.ctx(), connection);
        egui::Frame::central_panel(ui.style())
            .inner_margin(0)
            .show(ui, |ui| self.draw(ui));
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
        let profile = saved.selected_profile().expect("the server is saved");
        assert_eq!(
            (profile.name.as_str(), profile.server.as_str()),
            ("Test world", "example.org"),
            "saved under the server's own name"
        );
        assert_eq!(saved.token(), Some("token"));
        assert_eq!(app.name_input, "Test world");
        app.apply(Event::SessionExpired);
        assert_eq!(
            (app.status().tone, app.status().word),
            (Tone::Fault, "login expired")
        );
        assert_eq!(app.settings.token(), None);
        app.apply(Event::Failed("server unreachable".to_owned()));
        assert_eq!(app.status(), &Status::error("server unreachable"));
    }

    #[test]
    fn a_saved_server_is_picked_renamed_and_removed() {
        let (_dir, mut app) = app();
        let api = LauncherApi::new("https://login.example.org".to_owned()).expect("api");
        app.apply(Event::Connected("example.org".to_owned(), api, info()));
        app.name_input = " ".to_owned();
        app.rename_selected();
        assert_eq!(
            app.name_input, "Test world",
            "an empty name gets the old one back"
        );
        app.name_input = "Main".to_owned();
        app.rename_selected();
        let saved = Settings::load(&app.settings_path).expect("load");
        assert_eq!(saved.profiles[0].name, "Main");
        app.server_input.clear();
        app.pick("example.org");
        assert_eq!(app.server_input, "example.org");
        assert_eq!(app.status().word, "connecting");
        app.job = None;
        app.remove_selected();
        assert_eq!(app.status(), &Status::not_connected());
        assert!(app.connection.is_none() && app.server_input.is_empty());
        let saved = Settings::load(&app.settings_path).expect("load");
        assert!(saved.profiles.is_empty() && saved.selected.is_none());
    }

    #[test]
    fn the_launcher_starts_on_the_selected_saved_server() {
        let dir = tempfile::tempdir().expect("dir");
        let mut settings = Settings::default();
        settings.connected("example.org", "Test world");
        settings.connected("local.example.org", "Local world");
        settings.connected("example.org", "Test world");
        let app = LauncherApp::new(dir.path().join("settings.json"), settings, Found::default());
        assert_eq!(
            (app.server_input.as_str(), app.name_input.as_str()),
            ("example.org", "Test world")
        );
        assert_eq!(app.status().word, "connecting");
    }

    #[test]
    fn tones_use_the_website_state_colours() {
        let palette = &theme::NIGHT;
        assert_eq!(Tone::Ready.color(palette), palette.good);
        assert_eq!(Tone::Busy.color(palette), palette.warn);
        assert_eq!(Tone::Fault.color(palette), palette.bad);
        assert_eq!(Tone::Idle.color(palette), palette.ink_2);
    }

    #[test]
    fn state_words_start_with_a_capital() {
        assert_eq!(capitalized("logged in"), "Logged in");
        assert_eq!(capitalized(""), "");
    }

    #[test]
    fn home_opens_first_and_every_page_has_a_link() {
        let (_dir, app) = app();
        assert_eq!(app.page, Page::Home);
        let labels: Vec<_> = Page::ALL.iter().map(|page| page.label()).collect();
        assert_eq!(labels, ["Home", "Options", "Settings"]);
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
