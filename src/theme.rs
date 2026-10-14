use gpui_kit::App;
use gpui_kit::component::{
    Theme, ThemeMode,
    theme::{Colorize, oklch},
};

pub fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        let foreground = oklch(0.985, 0.0, 0.0);
        let card = oklch(0.212, 0.019, 322.12);
        let primary = oklch(0.455, 0.188, 13.697);
        let primary_foreground = oklch(0.969, 0.015, 12.422);
        let secondary = oklch(0.274, 0.006, 286.033);
        let muted = oklch(0.263, 0.024, 320.12);
        let border = oklch(1.0, 0.0, 0.0).opacity(0.1);
        let ring = oklch(0.542, 0.034, 322.5);

        theme.background = oklch(0.145, 0.008, 326.0);
        theme.foreground = foreground;
        theme.group_box = card;
        theme.group_box_foreground = foreground;
        theme.popover = card;
        theme.popover_foreground = foreground;
        theme.primary = primary;
        theme.primary_foreground = primary_foreground;
        theme.secondary = secondary;
        theme.secondary_foreground = foreground;
        theme.muted = muted;
        theme.muted_foreground = oklch(0.711, 0.019, 323.02);
        theme.accent = muted;
        theme.accent_foreground = foreground;
        theme.danger = oklch(0.704, 0.191, 22.216);
        theme.danger_foreground = theme.background;
        theme.border = border;
        theme.input = oklch(1.0, 0.0, 0.0).opacity(0.15);
        theme.ring = ring;

        theme.chart_1 = oklch(0.81, 0.117, 11.638);
        theme.chart_2 = oklch(0.645, 0.246, 16.439);
        theme.chart_3 = oklch(0.586, 0.253, 17.585);
        theme.chart_4 = oklch(0.514, 0.222, 16.935);
        theme.chart_5 = primary;

        theme.sidebar = oklch(0.21, 0.006, 285.885);
        theme.sidebar_foreground = foreground;
        theme.sidebar_primary = theme.chart_2;
        theme.sidebar_primary_foreground = primary_foreground;
        theme.sidebar_accent = muted;
        theme.sidebar_accent_foreground = foreground;
        theme.sidebar_border = border;
        // Sidebar focus uses the shared ring, which matches the supplied sidebar ring.

        // Kit has separate component colors in addition to its semantic palette.
        theme.primary_hover = theme.chart_4;
        theme.primary_active = primary.darken(0.1);
        theme.button_primary = primary;
        theme.button_primary_foreground = primary_foreground;
        theme.button_primary_hover = theme.primary_hover;
        theme.button_primary_active = theme.primary_active;
        theme.secondary_hover = muted;
        theme.secondary_active = muted.darken(0.1);
        theme.button_secondary = secondary;
        theme.button_secondary_foreground = foreground;
        theme.button_secondary_hover = theme.secondary_hover;
        theme.button_secondary_active = theme.secondary_active;
        theme.button = secondary;
        theme.button_foreground = foreground;
        theme.button_hover = muted;
        theme.button_active = theme.secondary_active;
        theme.slider_bar = primary;
        theme.slider_thumb = primary_foreground;
        theme.progress_bar = primary;
        theme.selection = primary.opacity(0.3);
        theme.title_bar = theme.background;
        theme.title_bar_border = border;
        theme.status_bar = card;
        theme.status_bar_border = border;
    });
}
