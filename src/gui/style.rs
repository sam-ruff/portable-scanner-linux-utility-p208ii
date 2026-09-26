//! Look and feel modelled on shadcn/ui's zinc theme, for light and dark mode.

use iced::border::Radius;
use iced::widget::{button, container, scrollable, text_input, toggler};
use iced::{Background, Border, Color, Shadow, Theme, Vector};

pub const RADIUS: f32 = 8.0;
pub const RADIUS_SMALL: f32 = 6.0;

const fn rgb(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub background: Color,
    pub foreground: Color,
    pub card: Color,
    pub muted: Color,
    pub muted_foreground: Color,
    pub border: Color,
    pub ring: Color,
    pub primary: Color,
    pub primary_foreground: Color,
    pub accent: Color,
    pub destructive: Color,
    pub success: Color,
    pub warning: Color,
    pub info: Color,
    is_dark: bool,
}

const LIGHT: Tokens = Tokens {
    background: rgb(0xfafafa),
    foreground: rgb(0x09090b),
    card: rgb(0xffffff),
    muted: rgb(0xf4f4f5),
    muted_foreground: rgb(0x71717a),
    border: rgb(0xe4e4e7),
    ring: rgb(0xa1a1aa),
    primary: rgb(0x18181b),
    primary_foreground: rgb(0xfafafa),
    accent: rgb(0xf4f4f5),
    destructive: rgb(0xdc2626),
    success: rgb(0x16a34a),
    warning: rgb(0xd97706),
    info: rgb(0x2563eb),
    is_dark: false,
};

const DARK: Tokens = Tokens {
    background: rgb(0x09090b),
    foreground: rgb(0xfafafa),
    card: rgb(0x111113),
    muted: rgb(0x27272a),
    muted_foreground: rgb(0xa1a1aa),
    border: rgb(0x27272a),
    ring: rgb(0x71717a),
    primary: rgb(0xfafafa),
    primary_foreground: rgb(0x18181b),
    accent: rgb(0x27272a),
    destructive: rgb(0xef4444),
    success: rgb(0x22c55e),
    warning: rgb(0xf59e0b),
    info: rgb(0x60a5fa),
    is_dark: true,
};

pub fn tokens(theme: &Theme) -> Tokens {
    if theme.extended_palette().is_dark {
        DARK
    } else {
        LIGHT
    }
}

fn border(color: Color, radius: f32) -> Border {
    Border {
        color,
        width: 1.0,
        radius: radius.into(),
    }
}

fn soft_shadow(t: &Tokens) -> Shadow {
    Shadow {
        color: Color::BLACK.scale_alpha(if t.is_dark { 0.4 } else { 0.06 }),
        offset: Vector::new(0.0, 1.0),
        blur_radius: 3.0,
    }
}

pub fn page(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.foreground),
        background: Some(t.background.into()),
        ..container::Style::default()
    }
}

pub fn card(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.foreground),
        background: Some(t.card.into()),
        border: border(t.border, RADIUS),
        shadow: soft_shadow(&t),
        snap: true,
    }
}

pub fn inset(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.foreground),
        background: Some(t.muted.scale_alpha(0.5).into()),
        border: border(t.border, RADIUS_SMALL),
        ..container::Style::default()
    }
}

pub fn alert(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.foreground),
        background: Some(t.destructive.scale_alpha(0.08).into()),
        border: border(t.destructive.scale_alpha(0.5), RADIUS),
        ..container::Style::default()
    }
}

/// A keyboard key hint, drawn on top of a button.
pub fn kbd(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: None,
        background: Some(t.muted_foreground.scale_alpha(0.18).into()),
        border: border(t.muted_foreground.scale_alpha(0.35), 4.0),
        ..container::Style::default()
    }
}

pub fn dot(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(color.into()),
        border: Border {
            radius: Radius::new(999.0),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

pub fn muted_text(theme: &Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(tokens(theme).muted_foreground),
    }
}

pub fn success_text(theme: &Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(tokens(theme).success),
    }
}

pub fn destructive_text(theme: &Theme) -> iced::widget::text::Style {
    iced::widget::text::Style {
        color: Some(tokens(theme).destructive),
    }
}

fn button_style(
    background: Option<Color>,
    text: Color,
    edge: Option<Color>,
    shadow: Shadow,
) -> button::Style {
    button::Style {
        background: background.map(Background::Color),
        text_color: text,
        border: Border {
            color: edge.unwrap_or(Color::TRANSPARENT),
            width: if edge.is_some() { 1.0 } else { 0.0 },
            radius: RADIUS_SMALL.into(),
        },
        shadow,
        snap: true,
    }
}

fn disabled(style: button::Style) -> button::Style {
    button::Style {
        background: style.background.map(|b| b.scale_alpha(0.5)),
        text_color: style.text_color.scale_alpha(0.5),
        shadow: Shadow::default(),
        ..style
    }
}

fn filled(base: Color, text: Color, theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let style = button_style(Some(base), text, None, soft_shadow(&t));
    match status {
        button::Status::Hovered => button::Style {
            background: Some(base.scale_alpha(0.9).into()),
            ..style
        },
        button::Status::Pressed => button::Style {
            background: Some(base.scale_alpha(0.8).into()),
            shadow: Shadow::default(),
            ..style
        },
        button::Status::Disabled => disabled(style),
        button::Status::Active => style,
    }
}

pub fn primary(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    filled(t.primary, t.primary_foreground, theme, status)
}

pub fn outline(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let style = button_style(Some(t.card), t.foreground, Some(t.border), soft_shadow(&t));
    match status {
        button::Status::Hovered | button::Status::Pressed => button::Style {
            background: Some(t.accent.into()),
            ..style
        },
        button::Status::Disabled => disabled(style),
        button::Status::Active => style,
    }
}

/// Underlined-on-hover text, for opening a file.
pub fn link(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let style = button_style(None, t.foreground, None, Shadow::default());
    match status {
        button::Status::Hovered | button::Status::Pressed => button::Style {
            background: Some(t.accent.into()),
            ..style
        },
        _ => style,
    }
}

pub fn ghost(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let style = button_style(None, t.foreground, None, Shadow::default());
    match status {
        button::Status::Hovered | button::Status::Pressed => button::Style {
            background: Some(t.accent.into()),
            ..style
        },
        button::Status::Disabled => disabled(style),
        button::Status::Active => style,
    }
}

pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let t = tokens(theme);
    let edge = match status {
        text_input::Status::Focused { .. } => t.ring,
        _ => t.border,
    };
    let (value, background) = match status {
        text_input::Status::Disabled => (t.muted_foreground, t.muted.scale_alpha(0.5)),
        _ => (t.foreground, t.card),
    };
    text_input::Style {
        background: background.into(),
        border: border(edge, RADIUS_SMALL),
        icon: t.muted_foreground,
        placeholder: t.muted_foreground,
        value,
        selection: t.ring.scale_alpha(0.4),
    }
}

/// Tinted band holding a card's title.
pub fn card_header(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.foreground),
        background: Some(
            t.muted
                .scale_alpha(if t.is_dark { 0.45 } else { 0.6 })
                .into(),
        ),
        border: Border {
            radius: Radius {
                top_left: RADIUS,
                top_right: RADIUS,
                bottom_right: 0.0,
                bottom_left: 0.0,
            },
            ..Border::default()
        },
        ..container::Style::default()
    }
}

pub fn segment_track(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.muted.into()),
        border: Border {
            radius: RADIUS.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// One option in a segmented control; the chosen one is raised like a tab.
pub fn segment(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let raised = if t.is_dark { t.background } else { t.card };
        let text_color = match (active, status) {
            (true, _) | (false, button::Status::Hovered | button::Status::Pressed) => t.foreground,
            _ => t.muted_foreground,
        };
        button::Style {
            background: active.then_some(raised.into()),
            text_color,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: RADIUS_SMALL.into(),
            },
            shadow: if active {
                soft_shadow(&t)
            } else {
                Shadow::default()
            },
            snap: true,
        }
    }
}

pub fn tooltip(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.primary_foreground),
        background: Some(t.primary.into()),
        border: Border {
            radius: RADIUS_SMALL.into(),
            ..Border::default()
        },
        shadow: Shadow {
            color: Color::BLACK.scale_alpha(if t.is_dark { 0.5 } else { 0.15 }),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 8.0,
        },
        snap: true,
    }
}

pub fn switch(theme: &Theme, status: toggler::Status) -> toggler::Style {
    let t = tokens(theme);
    let (on, hovered, enabled) = match status {
        toggler::Status::Active { is_toggled } => (is_toggled, false, true),
        toggler::Status::Hovered { is_toggled } => (is_toggled, true, true),
        toggler::Status::Disabled { is_toggled } => (is_toggled, false, false),
    };
    let track = if on { t.primary } else { t.border };
    let track = if hovered {
        track.scale_alpha(0.85)
    } else {
        track
    };
    let knob = match (on, t.is_dark) {
        (true, _) => t.primary_foreground,
        (false, true) => t.foreground,
        (false, false) => rgb(0xffffff),
    };
    let alpha = if enabled { 1.0 } else { 0.5 };
    toggler::Style {
        background: track.scale_alpha(alpha).into(),
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: knob.scale_alpha(alpha).into(),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: Some(t.foreground),
        border_radius: None,
        padding_ratio: 0.12,
    }
}

pub fn log_scroll(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let t = tokens(theme);
    let base = scrollable::default(theme, status);
    let scroller_colour = match status {
        scrollable::Status::Dragged { .. } | scrollable::Status::Hovered { .. } => t.ring,
        scrollable::Status::Active { .. } => t.border,
    };
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: scroller_colour.into(),
            border: Border {
                radius: Radius::new(999.0),
                ..Border::default()
            },
        },
    };
    scrollable::Style {
        vertical_rail: rail,
        horizontal_rail: rail,
        ..base
    }
}
