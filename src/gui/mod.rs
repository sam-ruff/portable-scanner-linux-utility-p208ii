//! Desktop app for scanning a pile of receipts into one folder.

mod logging;
mod style;

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use iced::futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{
    button, column, container, responsive, row, scrollable, space, svg, text, text_input, toggler,
    tooltip,
};
use iced::{Alignment, Element, Font, Length, Size, Task, window};
use log::{error, info};

pub use logging::init as init_logging;

use crate::encode::FileFormat;
use crate::params::{ColourMode, ScanSettings};
use crate::session::{Backend, Command, Event, EventSink, SessionOptions, run_worker};

const MAX_LOG_LINES: usize = 2000;
const PAPER_POLL_INTERVAL: Duration = Duration::from_millis(500);
const COMPACT_WIDTH: f32 = 480.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dpi(pub u16);

impl EventSink for UnboundedSender<Event> {
    fn emit(&self, event: Event) {
        // The window may have closed while the worker was busy
        let _ = self.unbounded_send(event);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperWidth {
    Full,
    Receipt80,
    Receipt58,
}

impl PaperWidth {
    fn millimetres(self) -> Option<u32> {
        match self {
            PaperWidth::Full => None,
            PaperWidth::Receipt80 => Some(80),
            PaperWidth::Receipt58 => Some(58),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Main,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Idle,
    Connecting,
    WaitingForPaper,
    Preparing,
    Scanning,
    Stopping,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Idle => "Ready",
            Status::Connecting => "Connecting",
            Status::WaitingForPaper => "Waiting for paper",
            Status::Preparing => "Calibrating",
            Status::Scanning => "Scanning",
            Status::Stopping => "Stopping",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Status::Idle => "Press Start, then feed receipts in one at a time.",
            Status::Connecting => "Looking for the scanner.",
            Status::WaitingForPaper => "Feed the next receipt into the scanner.",
            Status::Preparing => "Calibrating takes a few seconds the first time.",
            Status::Scanning => "Keep the receipt straight while it feeds.",
            Status::Stopping => "Finishing the current receipt.",
        }
    }

    fn colour(self, t: &style::Tokens) -> iced::Color {
        match self {
            Status::Idle => t.muted_foreground,
            Status::WaitingForPaper => t.success,
            Status::Connecting | Status::Preparing | Status::Scanning => t.info,
            Status::Stopping => t.muted_foreground,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    OutputDirChanged(String),
    Browse,
    Browsed(Option<PathBuf>),
    OpenFolder,
    Start,
    Stop,
    Worker(Event),
    Log(String),
    OpenSettings,
    CloseSettings,
    ModeSelected(ColourMode),
    DpiSelected(Dpi),
    PaperWidthSelected(PaperWidth),
    DuplexToggled(bool),
    AdvancedToggled(bool),
    FormatSelected(FileFormat),
    SmartCropToggled(bool),
    ClearLogs,
    DismissError,
    OpenLastFile,
    Key(keyboard::Event),
}

pub struct App {
    backend: Backend,
    output_dir: String,
    mode: ColourMode,
    dpi: Dpi,
    paper_width: PaperWidth,
    duplex: bool,
    page: Page,
    advanced: bool,
    format: FileFormat,
    smart_crop: bool,
    logs: VecDeque<String>,
    status: Status,
    worker: Option<mpsc::Sender<Command>>,
    saved: usize,
    last_saved: Option<PathBuf>,
    /// Receipts saved by the session that just ended, for the summary line.
    finished_session: Option<usize>,
    scanner_model: Option<String>,
    error: Option<String>,
}

/// Replaces the home directory with `~` for display.
pub fn abbreviate_home(path: &Path, home: Option<&Path>) -> String {
    let Some(rest) = home.and_then(|home| path.strip_prefix(home).ok()) else {
        return path.display().to_string();
    };
    if rest.as_os_str().is_empty() {
        return "~".into();
    }
    format!("~/{}", rest.display())
}

pub fn expand_home(text: &str, home: Option<&Path>) -> PathBuf {
    let text = text.trim();
    match (text.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(text),
    }
}

impl App {
    pub fn new(backend: Backend, output_dir: PathBuf) -> Self {
        let defaults = ScanSettings::default();
        Self {
            backend,
            output_dir: abbreviate_home(&output_dir, dirs::home_dir().as_deref()),
            mode: defaults.mode,
            dpi: Dpi(defaults.dpi),
            paper_width: PaperWidth::Full,
            duplex: defaults.duplex,
            page: Page::Main,
            advanced: false,
            format: FileFormat::default(),
            smart_crop: true,
            logs: VecDeque::new(),
            status: Status::Idle,
            worker: None,
            saved: 0,
            last_saved: None,
            finished_session: None,
            scanner_model: None,
            error: None,
        }
    }

    fn is_running(&self) -> bool {
        self.worker.is_some()
    }

    fn output_path(&self) -> PathBuf {
        expand_home(&self.output_dir, dirs::home_dir().as_deref())
    }

    fn settings(&self) -> ScanSettings {
        ScanSettings {
            mode: self.mode,
            dpi: self.dpi.0,
            duplex: self.duplex,
            page_width_mm: self.paper_width.millimetres(),
            ..ScanSettings::default()
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::OutputDirChanged(dir) => self.output_dir = dir,
            Message::Browse => {
                let dialog = rfd::AsyncFileDialog::new()
                    .set_title("Choose where scans are saved")
                    .set_directory(self.output_path())
                    .pick_folder();
                return Task::perform(dialog, |folder| {
                    Message::Browsed(folder.map(|f| f.path().to_path_buf()))
                });
            }
            Message::Browsed(Some(dir)) => {
                self.output_dir = abbreviate_home(&dir, dirs::home_dir().as_deref());
            }
            Message::Browsed(None) => {}
            Message::OpenFolder => self.open_folder(),
            Message::Start => return self.start(),
            Message::Stop => self.stop(),
            Message::Worker(event) => self.handle_event(event),
            Message::Log(line) => self.push_log(line),
            Message::OpenSettings => self.page = Page::Settings,
            Message::CloseSettings => self.page = Page::Main,
            Message::ModeSelected(mode) => self.mode = mode,
            Message::DpiSelected(dpi) => self.dpi = dpi,
            Message::PaperWidthSelected(width) => self.paper_width = width,
            Message::DuplexToggled(duplex) => self.duplex = duplex,
            Message::AdvancedToggled(advanced) => self.advanced = advanced,
            Message::FormatSelected(format) => self.format = format,
            Message::SmartCropToggled(on) => self.smart_crop = on,
            Message::ClearLogs => self.logs.clear(),
            Message::DismissError => self.error = None,
            Message::OpenLastFile => {
                if let Some(path) = self.last_saved.clone() {
                    self.open_with_system(&path);
                }
            }
            Message::Key(keyboard::Event::KeyPressed { key, .. }) => {
                return match (key.as_ref(), self.page, self.is_running()) {
                    (Key::Named(Named::Escape), Page::Settings, _) => {
                        self.page = Page::Main;
                        Task::none()
                    }
                    (_, Page::Settings, _) => Task::none(),
                    (Key::Named(Named::Space), Page::Main, false) => self.start(),
                    (Key::Named(Named::Space | Named::Escape), Page::Main, true) => {
                        self.stop();
                        Task::none()
                    }
                    _ => Task::none(),
                };
            }
            Message::Key(_) => {}
        }
        Task::none()
    }

    fn start(&mut self) -> Task<Message> {
        if self.is_running() {
            return Task::none();
        }
        let output_dir = self.output_path();
        if output_dir.as_os_str().is_empty() {
            self.error = Some("Choose a folder to save scans in.".into());
            return Task::none();
        }

        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = unbounded();
        let backend = self.backend.clone();
        let options = SessionOptions {
            settings: self.settings(),
            continuous: true,
            poll_interval: PAPER_POLL_INTERVAL,
            format: self.format,
            smart_crop: self.smart_crop,
        };
        let spawned = std::thread::Builder::new()
            .name("scanner".into())
            .spawn(move || run_worker(backend, output_dir, options, command_rx, event_tx));
        if let Err(err) = spawned {
            self.error = Some(format!("Could not start scanning: {err}"));
            return Task::none();
        }

        self.worker = Some(command_tx);
        self.status = Status::Connecting;
        self.error = None;
        self.saved = 0;
        self.last_saved = None;
        self.finished_session = None;
        Task::run(event_rx, Message::Worker)
    }

    fn stop(&mut self) {
        let Some(worker) = &self.worker else {
            return;
        };
        if worker.send(Command::Stop).is_err() {
            // The worker has already gone, so there is nothing left to stop
            self.worker = None;
            self.status = Status::Idle;
            return;
        }
        self.status = Status::Stopping;
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Connected { model, firmware } => {
                info!("Using {model} (firmware {firmware})");
                self.scanner_model = Some(model);
            }
            Event::WaitingForPaper if self.status != Status::Stopping => {
                self.status = Status::WaitingForPaper;
            }
            Event::Preparing if self.status != Status::Stopping => self.status = Status::Preparing,
            Event::Scanning if self.status != Status::Stopping => self.status = Status::Scanning,
            Event::WaitingForPaper | Event::Preparing | Event::Scanning => {}
            Event::Saved { paths } => {
                self.saved += 1;
                self.last_saved = paths.into_iter().next();
            }
            Event::Finished { saved } => {
                self.finished_session = Some(saved);
                self.finish(None);
            }
            Event::Failed { message } => self.finish(Some(message)),
        }
    }

    fn finish(&mut self, error: Option<String>) {
        self.worker = None;
        self.status = Status::Idle;
        self.error = error;
    }

    fn push_log(&mut self, line: String) {
        if self.logs.len() == MAX_LOG_LINES {
            self.logs.pop_front();
        }
        self.logs.push_back(line);
    }

    fn open_folder(&mut self) {
        let dir = self.output_path();
        if !dir.is_dir() {
            self.error = Some(format!(
                "{} doesn't exist yet. It is created when you start scanning.",
                self.output_dir
            ));
            return;
        }
        self.open_with_system(&dir);
    }

    fn open_with_system(&mut self, path: &Path) {
        let opened = std::process::Command::new("xdg-open").arg(path).spawn();
        if let Err(err) = opened {
            error!("Could not open {}: {err}", path.display());
            self.error = Some(format!("Couldn't open {}: {err}", path.display()));
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let page = responsive(move |size| {
            let compact = size.width < COMPACT_WIDTH;
            let content = match self.page {
                Page::Main => self.main_page(compact),
                Page::Settings => self.settings_page(),
            };
            scrollable(
                container(content.spacing(16).max_width(640))
                    .padding(if compact { 16 } else { 24 })
                    .center_x(Length::Fill),
            )
            .style(style::log_scroll)
            .into()
        });

        container(page)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::page)
            .into()
    }

    fn main_page(&self, compact: bool) -> iced::widget::Column<'_, Message> {
        let subtitle = match &self.scanner_model {
            Some(model) => format!("Canon {model} connected"),
            None => "Canon imageFORMULA P-208II".into(),
        };
        let header = row![
            column![
                text("Receipt Scanner").size(20).font(SEMIBOLD),
                text(subtitle).size(13).style(style::muted_text),
            ]
            .spacing(2)
            .width(Length::Fill),
            icon_button(SETTINGS_ICON, Message::OpenSettings),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        let mut page = column![header];
        if let Some(message) = &self.error {
            page = page.push(error_alert(message));
        }
        page = page
            .push(self.session_card())
            .push(self.destination_card(compact));
        if self.advanced {
            page = page.push(self.log_card());
        }
        page
    }

    fn settings_page(&self) -> iced::widget::Column<'_, Message> {
        let header = row![
            icon_button(BACK_ICON, Message::CloseSettings),
            text("Settings").size(20).font(SEMIBOLD),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        column![
            header,
            self.settings_card(),
            self.output_card(),
            self.advanced_card()
        ]
    }

    fn advanced_card(&self) -> Element<'_, Message> {
        titled_card(
            "Troubleshooting",
            None,
            setting_row(
                "Advanced mode",
                "Shows the driver log on the main page.",
                toggler(self.advanced)
                    .size(20)
                    .style(style::switch)
                    .on_toggle(Message::AdvancedToggled)
                    .into(),
                false,
            ),
        )
    }

    fn session_card(&self) -> Element<'_, Message> {
        let running = self.is_running();
        let status = self.status;
        let heading = row![
            container(space())
                .width(Length::Fixed(10.0))
                .height(Length::Fixed(10.0))
                .style(move |theme| style::dot(status.colour(&style::tokens(theme)))(theme)),
            text(status.label()).size(18).font(SEMIBOLD),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let count = column![
            text(self.saved.to_string()).size(24).font(SEMIBOLD),
            text("saved this session").size(12).style(style::muted_text),
        ]
        .align_x(Alignment::End);

        let mut body = column![
            row![heading, space().width(Length::Fill), count].align_y(Alignment::Center),
            text(self.status.hint()).size(14).style(style::muted_text),
        ]
        .spacing(6);

        if let Some(name) = self
            .last_saved
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|name| name.to_string_lossy().into_owned())
        {
            body = body.push(
                row![
                    text("Latest").size(13).style(style::muted_text),
                    button(text(name).size(13).font(Font::MONOSPACE))
                        .padding([2, 6])
                        .style(style::link)
                        .on_press(Message::OpenLastFile),
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            );
        }

        if let (Some(saved), false) = (self.finished_session, running) {
            let summary = match saved {
                0 => "Stopped. No receipts were scanned.".to_string(),
                1 => "Done. 1 receipt saved.".to_string(),
                n => format!("Done. {n} receipts saved."),
            };
            body = body.push(text(summary).size(14).style(style::success_text));
        }

        let action = match (running, status) {
            (true, Status::Stopping) => action_button("Stopping", None, style::outline, None),
            (true, _) => action_button(
                "Stop scanning",
                Some("Esc"),
                style::outline,
                Some(Message::Stop),
            ),
            (false, _) => action_button(
                "Start scanning",
                Some("Space"),
                style::primary,
                Some(Message::Start),
            ),
        };

        card(column![body, action].spacing(20))
    }

    fn destination_card(&self, compact: bool) -> Element<'_, Message> {
        let running = self.is_running();
        let input = text_input("Folder for scans", &self.output_dir)
            .on_input_maybe((!running).then_some(Message::OutputDirChanged))
            .on_submit(Message::Start)
            .padding([8, 12])
            .size(14)
            .style(style::input)
            .width(Length::Fill);
        let buttons = row![
            small_button(
                "Browse",
                style::outline,
                (!running).then_some(Message::Browse)
            ),
            small_button("Open folder", style::outline, Some(Message::OpenFolder)),
        ]
        .spacing(8);

        let controls: Element<'_, Message> = if compact {
            column![input, buttons].spacing(8).into()
        } else {
            row![input, buttons]
                .spacing(8)
                .align_y(Alignment::Center)
                .into()
        };
        titled_card(
            "Save to",
            None,
            column![
                text(format!(
                    "Each receipt is saved here as a numbered {}.",
                    self.format
                ))
                .size(13)
                .style(style::muted_text),
                controls,
            ]
            .spacing(12),
        )
    }

    fn settings_card(&self) -> Element<'_, Message> {
        let locked = self.is_running();
        let switch = |on: bool, message: fn(bool) -> Message| {
            toggler(on)
                .size(20)
                .style(style::switch)
                .on_toggle_maybe((!locked).then_some(message))
        };
        let colour = [
            Segment::new(ColourMode::Colour, "Colour", "Full colour. Largest files.")
                .icon(PALETTE_ICON),
            Segment::new(
                ColourMode::Grey,
                "Greyscale",
                "Shades of grey. Good for OCR.",
            )
            .icon(CONTRAST_ICON),
            Segment::new(
                ColourMode::BlackWhite,
                "B&W",
                "Pure black and white. Smallest files.",
            )
            .icon(TYPE_ICON),
        ];
        let resolution = [
            Segment::new(Dpi(150), "150", "Quick preview quality."),
            Segment::new(Dpi(200), "200", "Small files, readable text."),
            Segment::new(Dpi(300), "300", "Best for most receipts and OCR."),
            Segment::new(Dpi(600), "600", "Tiny print. Large files and slower."),
        ];
        let width = [
            Segment::new(PaperWidth::Full, "Full", "The whole feeder, 216 mm wide."),
            Segment::new(PaperWidth::Receipt80, "80 mm", "Standard till receipt."),
            Segment::new(
                PaperWidth::Receipt58,
                "58 mm",
                "Narrow card machine receipt.",
            ),
        ];

        let rows = column![
            setting_row(
                "Colour",
                "Greyscale keeps files small.",
                segmented(colour, self.mode, locked, Message::ModeSelected),
                true,
            ),
            divider(),
            setting_row(
                "Resolution",
                "Dots per inch. 300 suits most receipts.",
                segmented(resolution, self.dpi, locked, Message::DpiSelected),
                true,
            ),
            divider(),
            setting_row(
                "Paper width",
                "Narrower widths crop the sides of the scan, centred on the feeder.",
                segmented(width, self.paper_width, locked, Message::PaperWidthSelected),
                true,
            ),
            divider(),
            setting_row(
                "Scan both sides",
                "Also saves the back of each receipt.",
                switch(self.duplex, Message::DuplexToggled).into(),
                false,
            ),
        ]
        .spacing(16);

        titled_card("Scan settings", None, with_lock_note(rows, locked))
    }

    fn output_card(&self) -> Element<'_, Message> {
        let locked = self.is_running();
        let formats = [
            Segment::new(FileFormat::Png, "PNG", "Lossless. Best for OCR."),
            Segment::new(
                FileFormat::Jpeg,
                "JPEG",
                "Smallest files, slight blur on text.",
            ),
            Segment::new(FileFormat::Tiff, "TIFF", "Lossless, for archiving."),
            Segment::new(
                FileFormat::Pdf,
                "PDF",
                "One document per receipt, both sides as pages.",
            ),
        ];
        let rows = column![
            setting_row(
                "File format",
                "Every receipt is saved as its own file.",
                segmented(formats, self.format, locked, Message::FormatSelected),
                true,
            ),
            divider(),
            setting_row(
                "Smart crop",
                "Trims the scanner background from around the receipt.",
                toggler(self.smart_crop)
                    .size(20)
                    .style(style::switch)
                    .on_toggle_maybe((!locked).then_some(Message::SmartCropToggled))
                    .into(),
                false,
            ),
        ]
        .spacing(16);

        titled_card("Output", None, with_lock_note(rows, locked))
    }

    fn log_card(&self) -> Element<'_, Message> {
        let body: Element<'_, Message> = if self.logs.is_empty() {
            text("Nothing logged yet.")
                .size(12)
                .style(style::muted_text)
                .into()
        } else {
            column(self.logs.iter().map(|line| log_line(line)))
                .spacing(4)
                .into()
        };

        titled_card(
            "Activity log",
            Some(small_button(
                "Clear",
                style::ghost,
                (!self.logs.is_empty()).then_some(Message::ClearLogs),
            )),
            container(
                scrollable(container(body).padding(12).width(Length::Fill))
                    .anchor_bottom()
                    .style(style::log_scroll)
                    .height(Length::Fixed(240.0)),
            )
            .style(style::inset),
        )
    }
}

const SEMIBOLD: Font = Font {
    weight: iced::font::Weight::Semibold,
    ..Font::DEFAULT
};

type ButtonStyle = fn(&iced::Theme, button::Status) -> button::Style;

fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(24)
        .width(Length::Fill)
        .style(style::card)
        .into()
}

/// Adds a note above settings that can't change while scanning.
fn with_lock_note<'a>(body: impl Into<Element<'a, Message>>, locked: bool) -> Element<'a, Message> {
    if !locked {
        return body.into();
    }
    column![
        text("Stop scanning to change these.")
            .size(13)
            .style(style::muted_text),
        body.into(),
    ]
    .spacing(16)
    .into()
}

/// A card whose title sits in its own tinted band above the content.
fn titled_card<'a>(
    title: &'a str,
    action: Option<Element<'a, Message>>,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut heading = row![text(title).size(16).font(SEMIBOLD).width(Length::Fill)]
        .align_y(Alignment::Center)
        .height(Length::Fixed(32.0));
    if let Some(action) = action {
        heading = heading.push(action);
    }
    container(column![
        container(heading)
            .padding([8, 24])
            .width(Length::Fill)
            .style(style::card_header),
        divider(),
        container(body).padding(24).width(Length::Fill),
    ])
    .width(Length::Fill)
    .style(style::card)
    .into()
}

const SETTINGS_ICON: &[u8] = include_bytes!("../../assets/icons/settings.svg");
const BACK_ICON: &[u8] = include_bytes!("../../assets/icons/arrow-left.svg");
const PALETTE_ICON: &[u8] = include_bytes!("../../assets/icons/palette.svg");
const CONTRAST_ICON: &[u8] = include_bytes!("../../assets/icons/contrast.svg");
const TYPE_ICON: &[u8] = include_bytes!("../../assets/icons/type.svg");

fn icon<'a>(bytes: &'static [u8], size: f32, muted: bool) -> Element<'a, Message> {
    svg(svg::Handle::from_memory(bytes))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |theme, _| {
            let t = style::tokens(theme);
            svg::Style {
                color: Some(if muted {
                    t.muted_foreground
                } else {
                    t.foreground
                }),
            }
        })
        .into()
}

fn with_tip<'a>(
    content: impl Into<Element<'a, Message>>,
    tip: &'a str,
    position: tooltip::Position,
) -> Element<'a, Message> {
    tooltip(
        content,
        container(text(tip).size(12))
            .padding([4, 8])
            .style(style::tooltip),
        position,
    )
    .gap(6)
    .into()
}

fn icon_button<'a>(bytes: &'static [u8], message: Message) -> Element<'a, Message> {
    let tip = match message {
        Message::OpenSettings => "Settings",
        _ => "Back (Esc)",
    };
    with_tip(
        button(icon(bytes, 18.0, false))
            .padding(9)
            .style(style::outline)
            .on_press(message),
        tip,
        tooltip::Position::Bottom,
    )
}

/// One option in a segmented control.
struct Segment<T> {
    value: T,
    label: &'static str,
    tip: &'static str,
    icon: Option<&'static [u8]>,
}

impl<T> Segment<T> {
    fn new(value: T, label: &'static str, tip: &'static str) -> Self {
        Self {
            value,
            label,
            tip,
            icon: None,
        }
    }

    fn icon(mut self, bytes: &'static [u8]) -> Self {
        self.icon = Some(bytes);
        self
    }
}

/// shadcn style tabs: a muted track with the chosen option raised.
fn segmented<'a, T: Copy + PartialEq + 'a>(
    segments: impl IntoIterator<Item = Segment<T>>,
    selected: T,
    locked: bool,
    on_select: fn(T) -> Message,
) -> Element<'a, Message> {
    let buttons = segments.into_iter().map(|segment| {
        let active = segment.value == selected;
        let mut label = row![].spacing(6).align_y(Alignment::Center);
        if let Some(bytes) = segment.icon {
            label = label.push(icon(bytes, 15.0, !active));
        }
        label = label.push(text(segment.label).size(13).font(SEMIBOLD));
        let choice = button(container(label).center_x(Length::Fill))
            .padding([6, 10])
            .width(Length::Fill)
            .style(style::segment(active))
            .on_press_maybe((!locked).then(|| on_select(segment.value)));
        with_tip(choice, segment.tip, tooltip::Position::Top)
    });
    container(row(buttons).spacing(4))
        .padding(4)
        .width(Length::Fill)
        .style(style::segment_track)
        .into()
}

/// Label and description beside a control, or above it when `stacked`.
fn setting_row<'a>(
    label: &'a str,
    description: &'a str,
    control: Element<'a, Message>,
    stacked: bool,
) -> Element<'a, Message> {
    let label = column![
        text(label).size(14).font(SEMIBOLD),
        text(description).size(13).style(style::muted_text),
    ]
    .spacing(2)
    .width(Length::Fill);
    if stacked {
        return column![label, control].spacing(8).into();
    }
    row![label, control]
        .spacing(16)
        .align_y(Alignment::Center)
        .into()
}

fn kbd(key: &str) -> Element<'_, Message> {
    container(text(key).size(11).font(Font::MONOSPACE))
        .padding([1, 6])
        .style(style::kbd)
        .into()
}

/// Splits a formatted log line into time, level and message.
fn split_log_line(line: &str) -> (&str, &str, &str) {
    let mut parts = line.trim_start().splitn(3, char::is_whitespace);
    let time = parts.next().unwrap_or_default();
    let level = parts.next().unwrap_or_default();
    let message = parts.next().unwrap_or_default().trim_start();
    (time, level, message)
}

fn log_line(line: &str) -> Element<'_, Message> {
    let (time, level, message) = split_log_line(line);
    let mut entry = row![
        text(time)
            .size(12)
            .font(Font::MONOSPACE)
            .width(Length::Fixed(56.0))
            .style(style::muted_text),
    ]
    .spacing(8);
    if level != "INFO" {
        let colour = move |theme: &iced::Theme| {
            let t = style::tokens(theme);
            iced::widget::text::Style {
                color: Some(match level {
                    "ERROR" => t.destructive,
                    "WARN" => t.warning,
                    _ => t.muted_foreground,
                }),
            }
        };
        entry = entry.push(text(level).size(12).font(Font::MONOSPACE).style(colour));
    }
    entry
        .push(
            text(message)
                .size(12)
                .font(Font::MONOSPACE)
                .width(Length::Fill),
        )
        .into()
}

fn divider<'a>() -> Element<'a, Message> {
    container(space())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|theme| container::Style {
            background: Some(style::tokens(theme).border.into()),
            ..container::Style::default()
        })
        .into()
}

fn small_button<'a>(
    label: &'a str,
    look: ButtonStyle,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    button(text(label).size(14))
        .padding([8, 14])
        .style(look)
        .on_press_maybe(on_press)
        .into()
}

fn action_button<'a>(
    label: &'a str,
    shortcut: Option<&'a str>,
    look: ButtonStyle,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let mut content = row![text(label).size(15).font(SEMIBOLD)]
        .spacing(10)
        .align_y(Alignment::Center);
    if let Some(key) = shortcut {
        content = content.push(kbd(key));
    }
    button(container(content).center_x(Length::Fill))
        .padding([12, 16])
        .width(Length::Fill)
        .style(look)
        .on_press_maybe(on_press)
        .into()
}

fn error_alert(message: &str) -> Element<'_, Message> {
    container(
        row![
            column![
                text("Couldn't scan")
                    .size(14)
                    .font(SEMIBOLD)
                    .style(style::destructive_text),
                text(message).size(14),
            ]
            .spacing(4)
            .width(Length::Fill),
            small_button("Dismiss", style::ghost, Some(Message::DismissError)),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding([14, 16])
    .width(Length::Fill)
    .style(style::alert)
    .into()
}

/// Opens the window. Log lines arrive through `logs` for the advanced view.
pub fn run(backend: Backend, output_dir: PathBuf, logs: UnboundedReceiver<String>) -> iced::Result {
    let logs = Cell::new(Some(logs));
    iced::application(
        move || {
            let app = App::new(backend.clone(), output_dir.clone());
            let task = logs
                .take()
                .map_or_else(Task::none, |logs| Task::run(logs, Message::Log));
            (app, task)
        },
        App::update,
        App::view,
    )
    .title("Receipt Scanner")
    .subscription(|_: &App| keyboard::listen().map(Message::Key))
    .window(window::Settings {
        size: Size::new(620.0, 760.0),
        min_size: Some(Size::new(360.0, 480.0)),
        ..window::Settings::default()
    })
    .run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::PaperSupply;

    fn app() -> App {
        App::new(
            Backend::Simulated {
                supply: PaperSupply::Sheets(0),
                read_delay: Duration::ZERO,
            },
            PathBuf::from("/tmp/p208ii-gui-test"),
        )
    }

    #[test]
    fn defaults_to_idle_with_folder() {
        let app = app();
        assert_eq!(app.status, Status::Idle);
        assert_eq!(app.output_dir, "/tmp/p208ii-gui-test");
        assert!(!app.is_running());
    }

    #[test]
    fn start_requires_a_folder() {
        let mut app = app();
        let _ = app.update(Message::OutputDirChanged("   ".into()));
        let _ = app.update(Message::Start);
        assert!(!app.is_running());
        assert!(app.error.is_some());
    }

    #[test]
    fn start_then_finish_returns_to_idle() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut app = app();
        let _ = app.update(Message::OutputDirChanged(dir.path().display().to_string()));
        let _ = app.update(Message::Start);
        assert!(app.is_running());
        assert_eq!(app.status, Status::Connecting);

        let _ = app.update(Message::Worker(Event::Saved {
            paths: vec![dir.path().join("scan-0001.png")],
        }));
        assert_eq!(app.saved, 1);

        let _ = app.update(Message::Stop);
        assert_eq!(app.status, Status::Stopping);
        let _ = app.update(Message::Worker(Event::Scanning));
        assert_eq!(app.status, Status::Stopping);

        let _ = app.update(Message::Worker(Event::Finished { saved: 1 }));
        assert!(!app.is_running());
        assert_eq!(app.status, Status::Idle);
        assert_eq!(app.error, None);
    }

    #[test]
    fn failure_is_shown() {
        let mut app = app();
        app.worker = Some(mpsc::channel().0);
        let _ = app.update(Message::Worker(Event::Failed {
            message: "paper jam".into(),
        }));
        assert!(!app.is_running());
        assert_eq!(app.error.as_deref(), Some("paper jam"));
    }

    #[test]
    fn stop_with_vanished_worker_resets() {
        let mut app = app();
        let (tx, rx) = mpsc::channel();
        drop(rx);
        app.worker = Some(tx);
        let _ = app.update(Message::Stop);
        assert!(!app.is_running());
    }

    #[test]
    fn logs_are_capped() {
        let mut app = app();
        for i in 0..MAX_LOG_LINES + 5 {
            let _ = app.update(Message::Log(format!("line {i}")));
        }
        assert_eq!(app.logs.len(), MAX_LOG_LINES);
        assert_eq!(app.logs.front().map(String::as_str), Some("line 5"));
    }

    #[test]
    fn settings_map_to_scan_settings() {
        let mut app = app();
        let _ = app.update(Message::ModeSelected(ColourMode::BlackWhite));
        let _ = app.update(Message::DpiSelected(Dpi(200)));
        let _ = app.update(Message::PaperWidthSelected(PaperWidth::Receipt80));
        let _ = app.update(Message::DuplexToggled(true));
        let settings = app.settings();
        assert_eq!(settings.mode, ColourMode::BlackWhite);
        assert_eq!(settings.dpi, 200);
        assert_eq!(settings.page_width_mm, Some(80));
        assert!(settings.duplex);
    }

    #[test]
    fn home_is_shown_as_tilde() {
        let home = Path::new("/home/sam");
        assert_eq!(
            abbreviate_home(Path::new("/home/sam/Pictures/Scans"), Some(home)),
            "~/Pictures/Scans"
        );
        assert_eq!(abbreviate_home(home, Some(home)), "~");
        assert_eq!(abbreviate_home(Path::new("/tmp/x"), Some(home)), "/tmp/x");
        assert_eq!(
            abbreviate_home(Path::new("/home/sammy"), Some(home)),
            "/home/sammy"
        );
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = Path::new("/home/sam");
        assert_eq!(
            expand_home(" ~/Pictures/Scans ", Some(home)),
            PathBuf::from("/home/sam/Pictures/Scans")
        );
        assert_eq!(expand_home("~", Some(home)), PathBuf::from("/home/sam"));
        assert_eq!(expand_home("~bob/x", Some(home)), PathBuf::from("~bob/x"));
        assert_eq!(expand_home("~/x", None), PathBuf::from("~/x"));
    }

    #[test]
    fn log_lines_split_into_parts() {
        assert_eq!(
            split_log_line("    12.3s WARN  Paper is skewed"),
            ("12.3s", "WARN", "Paper is skewed")
        );
        assert_eq!(split_log_line(""), ("", "", ""));
    }

    #[test]
    fn finished_session_is_summarised_until_next_start() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut app = app();
        let _ = app.update(Message::OutputDirChanged(dir.path().display().to_string()));
        let _ = app.update(Message::Start);
        let _ = app.update(Message::Worker(Event::Connected {
            model: "P-208II".into(),
            firmware: "1.0".into(),
        }));
        let _ = app.update(Message::Worker(Event::Finished { saved: 4 }));
        assert_eq!(app.finished_session, Some(4));
        assert_eq!(app.scanner_model.as_deref(), Some("P-208II"));

        let _ = app.update(Message::Start);
        assert_eq!(app.finished_session, None);
    }

    fn press(key: Key) -> Message {
        Message::Key(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::default(),
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn settings_page_opens_and_closes() {
        let mut app = app();
        let _ = app.update(Message::OpenSettings);
        assert_eq!(app.page, Page::Settings);
        let _ = app.update(Message::CloseSettings);
        assert_eq!(app.page, Page::Main);
    }

    #[test]
    fn escape_on_settings_goes_back_without_stopping() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut app = app();
        let _ = app.update(Message::OutputDirChanged(dir.path().display().to_string()));
        let _ = app.update(Message::Start);
        let _ = app.update(Message::OpenSettings);

        let _ = app.update(press(Key::Named(Named::Escape)));
        assert_eq!(app.page, Page::Main);
        assert_eq!(app.status, Status::Connecting);
    }

    #[test]
    fn space_does_nothing_on_settings_page() {
        let mut app = app();
        let _ = app.update(Message::OpenSettings);
        let _ = app.update(press(Key::Named(Named::Space)));
        assert!(!app.is_running());
    }

    #[test]
    fn space_starts_and_escape_stops() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut app = app();
        let _ = app.update(Message::OutputDirChanged(dir.path().display().to_string()));
        let _ = app.update(press(Key::Named(Named::Escape)));
        assert!(!app.is_running());

        let _ = app.update(press(Key::Named(Named::Space)));
        assert!(app.is_running());
        let _ = app.update(press(Key::Named(Named::Escape)));
        assert_eq!(app.status, Status::Stopping);
    }
}
