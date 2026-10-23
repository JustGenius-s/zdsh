use gpui::{App, Hsla, Rems, Styled, rems};

use crate::{ActiveTheme, theme_settings};

/// Text sizes for application controls, independent of their layout dimensions.
#[derive(Clone, Copy)]
pub enum UiTextSize {
    /// Navigation, list entries, buttons, and ordinary labels.
    Body,
    /// Supplementary descriptions and compact metadata.
    Caption,
}

impl UiTextSize {
    /// Scales with the user's UI font size, with 16px as the layout baseline.
    pub fn font_size(self) -> Rems {
        rems(match self {
            Self::Body => 14. / 16.,
            Self::Caption => 12. / 16.,
        })
    }

    /// An explicit line height prevents controls inheriting GPUI's paragraph spacing.
    pub fn line_height(self) -> Rems {
        rems(match self {
            Self::Body => 20. / 16.,
            Self::Caption => 16. / 16.,
        })
    }
}

/// Foreground roles shared by application controls.
#[derive(Clone, Copy)]
pub enum UiTextTone {
    /// Current selections and text requiring the strongest emphasis.
    Primary,
    /// Ordinary, unselected content.
    Secondary,
    /// Group labels, timestamps, descriptions, and empty states.
    Muted,
}

impl UiTextTone {
    /// Resolves the role from the active theme rather than a screenshot's RGB values.
    pub fn color(self, cx: &App) -> Hsla {
        let colors = cx.theme().colors();
        match self {
            Self::Primary => colors.text,
            // Muted is already 60% in Z themes; reusing it loses the ordinary-content tier.
            Self::Secondary => colors.text.opacity(0.74),
            Self::Muted => colors.text_muted,
        }
    }
}

/// Applies the application's typography to GPUI Kit components and base elements.
pub trait UiTypography: Styled + Sized {
    /// Keeps user font preferences and UI scaling while choosing explicit text semantics.
    fn ui_text(self, size: UiTextSize, tone: UiTextTone, cx: &App) -> Self {
        self.font(theme_settings(cx).ui_font(cx).clone())
            .text_size(size.font_size())
            .line_height(size.line_height())
            .text_color(tone.color(cx))
    }
}

impl<T: Styled> UiTypography for T {}
