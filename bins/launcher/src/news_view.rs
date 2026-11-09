// SPDX-License-Identifier: GPL-3.0-or-later
//! The news panel, in the look of the website's news list: the newest posts divided by rules,
//! each with an outlined kind badge, the local date, the title, a picture at panel width and the
//! first lines of the body; "Read more" opens the full post. A fresh downtime post is pinned at
//! the top inside a warning outline (the website's downtime note).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Utc;
use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, Margin, Rect, RichText, Stroke, TextStyle,
    TextureHandle, TextureOptions, pos2, text::LayoutJob, vec2,
};

use crate::api::LauncherApi;
use crate::news::{
    FEED_MIN_AGE, NewsCache, NewsState, Post, PostKind, REFRESH_INTERVAL, color_image, is_pinned,
    post_date, shown_posts,
};
use crate::news_html::{self, Block, BlockKind, Span};
use crate::theme::{
    CORNER, LABEL_STYLE, LINE_WIDTH, POST_TITLE_STYLE, Palette, SEMIBOLD, TITLE_STYLE,
};

/// Height of a picture in a closed post (a centre crop at panel width).
pub const PICTURE_HEIGHT: f32 = 200.0;
/// Body rows of a closed post.
const PREVIEW_ROWS: usize = 3;
/// The mark at the end of a cut preview.
const ELLIPSIS: char = '\u{2026}';
/// Space above and below a post, and inside the pinned outline.
const POST_GAP: f32 = 14.0;
const PINNED_PADDING: i8 = 12;
/// Space inside a badge.
const BADGE_MARGIN: Margin = Margin::symmetric(7, 1);
/// Width of a list marker column, per nesting level.
const LIST_INDENT: f32 = 18.0;
/// Repaint interval while a news worker runs.
const WORKER_REPAINT: Duration = Duration::from_millis(100);
/// The panel heading and the toggle texts.
const HEADING: &str = "News";
const READ_MORE: &str = "Read more";
const READ_LESS: &str = "Show less";

/// The part of a picture of `texture` size shown in a `frame` of another shape: the centre, as
/// wide or as high as the picture allows, in texture coordinates.
pub fn center_crop_uv(texture: egui::Vec2, frame: egui::Vec2) -> Rect {
    let texture_aspect = texture.x / texture.y;
    let frame_aspect = frame.x / frame.y;
    if texture_aspect > frame_aspect {
        let width = frame_aspect / texture_aspect;
        Rect::from_min_max(
            pos2((1.0 - width) / 2.0, 0.0),
            pos2((1.0 + width) / 2.0, 1.0),
        )
    } else {
        let height = texture_aspect / frame_aspect;
        Rect::from_min_max(
            pos2(0.0, (1.0 - height) / 2.0),
            pos2(1.0, (1.0 + height) / 2.0),
        )
    }
}

/// The badge colour of a post kind.
pub fn kind_color(kind: PostKind, palette: &Palette) -> Color32 {
    match kind {
        PostKind::Downtime => palette.warn,
        PostKind::News => palette.link,
        PostKind::PatchNotes => palette.good,
        PostKind::DevUpdate => palette.crystal,
        PostKind::Other => palette.ink_2,
    }
}

/// The news panel: the news state, the picture textures and the opened posts.
pub struct NewsPanel {
    state: NewsState,
    pictures: HashMap<i64, TextureHandle>,
    opened: HashSet<i64>,
}

impl NewsPanel {
    /// The panel with the news cached in `settings_dir`.
    pub fn open(settings_dir: PathBuf) -> Self {
        Self {
            state: NewsState::open(NewsCache::new(settings_dir)),
            pictures: HashMap::new(),
            opened: HashSet::new(),
        }
    }

    /// Asks for the news of a server that just connected, unless they were fetched less than
    /// [`FEED_MIN_AGE`] ago.
    pub fn connected(&mut self, api: &LauncherApi) {
        if self
            .state
            .refresh_due(api.host(), Instant::now(), FEED_MIN_AGE)
        {
            self.state.start_refresh(api.clone());
        }
    }

    /// Takes the worker results, makes the picture textures, and refreshes the news of the
    /// connected server every [`REFRESH_INTERVAL`].
    pub fn update(&mut self, ctx: &egui::Context, connection: Option<&LauncherApi>) {
        for (id, picture) in self.state.poll() {
            let texture = ctx.load_texture(
                format!("news picture {id}"),
                color_image(&picture),
                TextureOptions::LINEAR,
            );
            self.pictures.insert(id, texture);
        }
        let ids: HashSet<i64> = self
            .state
            .feed()
            .map(|feed| feed.posts.iter().map(|post| post.id).collect())
            .unwrap_or_default();
        self.pictures.retain(|id, _| ids.contains(id));
        self.opened.retain(|id| ids.contains(id));
        if let Some(api) = connection {
            let now = Instant::now();
            if self.state.refresh_due(api.host(), now, REFRESH_INTERVAL) {
                self.state.start_refresh(api.clone());
            }
            ctx.request_repaint_after(self.state.next_refresh(api.host(), now));
        }
        if self.state.busy() {
            ctx.request_repaint_after(WORKER_REPAINT);
        }
    }

    /// Draws the panel. Returns the error of a link that could not be opened.
    pub fn draw(&mut self, ui: &mut egui::Ui, palette: &Palette) -> Option<String> {
        ui.label(
            RichText::new(HEADING)
                .text_style(TextStyle::Name(TITLE_STYLE.into()))
                .color(palette.ink),
        );
        if let Some(line) = self.state.status_line() {
            ui.label(RichText::new(line).small().color(palette.ink_2));
        }
        let feed = self.state.feed()?;
        let now = Utc::now();
        let mut failure = None;
        let mut toggled = None;
        for post in shown_posts(feed, now) {
            let opened = self.opened.contains(&post.id);
            let picture = self.pictures.get(&post.id);
            let card = PostCard {
                post,
                picture,
                opened,
                palette,
            };
            let outcome = if is_pinned(post, now) {
                egui::Frame::new()
                    .stroke(Stroke::new(LINE_WIDTH, palette.warn))
                    .corner_radius(CornerRadius::same(CORNER))
                    .inner_margin(Margin::same(PINNED_PADDING))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        card.draw(ui)
                    })
                    .inner
            } else {
                let outcome = card.draw(ui);
                ui.add_space(POST_GAP);
                rule(ui, palette.rule);
                outcome
            };
            ui.add_space(POST_GAP);
            if outcome.toggled {
                toggled = Some(post.id);
            }
            failure = failure.or(outcome.failure);
        }
        if let Some(id) = toggled
            && !self.opened.remove(&id)
        {
            self.opened.insert(id);
        }
        failure
    }
}

/// A full-width 1px line.
fn rule(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), LINE_WIDTH), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, color);
}

/// What the player did in a post.
struct CardOutcome {
    /// "Read more" or "Show less" was pressed.
    toggled: bool,
    /// A link could not be opened.
    failure: Option<String>,
}

/// One post as drawn.
struct PostCard<'a> {
    post: &'a Post,
    picture: Option<&'a TextureHandle>,
    opened: bool,
    palette: &'a Palette,
}

impl PostCard<'_> {
    fn draw(&self, ui: &mut egui::Ui) -> CardOutcome {
        let palette = self.palette;
        ui.horizontal(|ui| {
            let color = kind_color(self.post.kind, palette);
            egui::Frame::new()
                .stroke(Stroke::new(LINE_WIDTH, color))
                .corner_radius(CornerRadius::same(CORNER))
                .inner_margin(BADGE_MARGIN)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(self.post.kind.label())
                            .text_style(TextStyle::Name(LABEL_STYLE.into()))
                            .color(color),
                    );
                });
            ui.label(
                RichText::new(post_date(self.post.created_at))
                    .small()
                    .color(palette.ink_2),
            );
        });
        ui.label(
            RichText::new(&self.post.title)
                .text_style(TextStyle::Name(POST_TITLE_STYLE.into()))
                .color(palette.ink),
        );
        if let Some(texture) = self.picture {
            self.draw_picture(ui, texture);
        }
        let blocks = news_html::render(&self.post.body_html);
        let failure = if self.opened {
            draw_blocks(ui, palette, &blocks)
        } else {
            draw_preview(ui, palette, &news_html::plain_text(&blocks));
            None
        };
        let toggle = if self.opened { READ_LESS } else { READ_MORE };
        let toggled = ui
            .add(egui::Link::new(RichText::new(toggle).color(palette.link)))
            .clicked();
        CardOutcome { toggled, failure }
    }

    /// The picture at panel width: a centre crop of [`PICTURE_HEIGHT`] in a closed post, the
    /// whole picture in an opened one.
    fn draw_picture(&self, ui: &mut egui::Ui, texture: &TextureHandle) {
        let width = ui.available_width();
        let size = texture.size_vec2();
        let (frame, uv) = if self.opened {
            (
                vec2(width, width * size.y / size.x),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            )
        } else {
            let frame = vec2(width, PICTURE_HEIGHT);
            (frame, center_crop_uv(size, frame))
        };
        ui.add(
            egui::Image::from_texture(texture)
                .uv(uv)
                .fit_to_exact_size(frame)
                .maintain_aspect_ratio(false)
                .corner_radius(CornerRadius::same(CORNER)),
        );
    }
}

/// The first [`PREVIEW_ROWS`] rows of `text`, cut with an ellipsis.
fn draw_preview(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    let mut job = LayoutJob::simple(
        text.to_owned(),
        TextStyle::Body.resolve(ui.style()),
        palette.ink_2,
        ui.available_width(),
    );
    job.wrap.max_rows = PREVIEW_ROWS;
    job.wrap.overflow_character = Some(ELLIPSIS);
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.label(galley);
}

/// The look of one text run.
fn span_text(span: &Span, palette: &Palette) -> RichText {
    let mut text = RichText::new(&span.text).color(palette.ink);
    if span.style.strong {
        text = text.family(FontFamily::Name(SEMIBOLD.into()));
    }
    if span.style.emphasis {
        text = text.italics();
    }
    if span.style.code {
        text = text.monospace().color(palette.crystal);
    }
    if span.link.is_some() {
        text = text.color(palette.link);
    }
    text
}

/// The runs of one block, wrapped; a link shows its address on hover and opens on a click.
fn draw_spans(ui: &mut egui::Ui, palette: &Palette, spans: &[Span]) -> Option<String> {
    let mut failure = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for span in spans {
            let text = span_text(span, palette);
            match &span.link {
                Some(url) => {
                    if ui.add(egui::Link::new(text)).on_hover_text(url).clicked()
                        && let Err(error) = open::that_detached(url)
                    {
                        failure = Some(format!("cannot open the link: {error}"));
                    }
                }
                None => {
                    ui.label(text);
                }
            }
        }
    });
    failure
}

/// The whole body: paragraphs, and list items with a hanging marker.
fn draw_blocks(ui: &mut egui::Ui, palette: &Palette, blocks: &[Block]) -> Option<String> {
    let mut failure = None;
    for block in blocks {
        let outcome = match &block.kind {
            BlockKind::Paragraph => draw_spans(ui, palette, &block.spans),
            BlockKind::Item { depth, marker } => {
                ui.horizontal(|ui| {
                    ui.add_space(LIST_INDENT * *depth as f32);
                    ui.add_sized(
                        vec2(LIST_INDENT, ui.text_style_height(&TextStyle::Body)),
                        egui::Label::new(RichText::new(marker).color(palette.ink_2)),
                    );
                    draw_spans(ui, palette, &block.spans)
                })
                .inner
            }
        };
        failure = failure.or(outcome);
    }
    failure
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DAY, NIGHT};

    #[test]
    fn a_wide_picture_is_cut_at_the_sides_and_a_tall_one_at_top_and_bottom() {
        let frame = vec2(400.0, 200.0);
        let wide = center_crop_uv(vec2(800.0, 200.0), frame);
        assert_eq!(wide, Rect::from_min_max(pos2(0.25, 0.0), pos2(0.75, 1.0)));
        let tall = center_crop_uv(vec2(400.0, 400.0), frame);
        assert_eq!(tall, Rect::from_min_max(pos2(0.0, 0.25), pos2(1.0, 0.75)));
        let same = center_crop_uv(vec2(800.0, 400.0), frame);
        assert_eq!(same, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
    }

    #[test]
    fn downtime_badges_use_the_warning_colour() {
        for palette in [&NIGHT, &DAY] {
            assert_eq!(kind_color(PostKind::Downtime, palette), palette.warn);
            assert_eq!(kind_color(PostKind::News, palette), palette.link);
        }
    }

    #[test]
    fn styled_runs_get_their_colours() {
        let span = |code: bool, link: Option<&str>| Span {
            text: "x".to_owned(),
            style: news_html::SpanStyle {
                strong: false,
                emphasis: false,
                code,
            },
            link: link.map(str::to_owned),
        };
        let color = |text: RichText| {
            let style = egui::Style::default();
            let mut job = LayoutJob::default();
            text.append_to(
                &mut job,
                &style,
                egui::FontSelection::Default,
                egui::Align::Min,
            );
            job.sections[0].format.color
        };
        assert_eq!(color(span_text(&span(false, None), &NIGHT)), NIGHT.ink);
        assert_eq!(color(span_text(&span(true, None), &NIGHT)), NIGHT.crystal);
        assert_eq!(
            color(span_text(
                &span(false, Some("https://x.example.org")),
                &NIGHT
            )),
            NIGHT.link
        );
    }
}
