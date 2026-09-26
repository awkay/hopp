//! Low-bandwidth ("turtle") toggle for the native call windows.

use iced::widget::{container, text, tooltip};
use iced::{Background, Border, Color, Element, Padding, Theme};
use socket_lib::BandwidthModeState;

use crate::components::split_button::{split_button_svg_sized, SplitButtonSize};
use crate::windows::colors::ColorToken;
use crate::windows::shadows::ShadowToken;

const ICON_TURTLE: &[u8] = include_bytes!("../../resources/icons/turtle.svg");

/// Outer width of the toggle at the given size.
pub const fn bandwidth_toggle_width(size: SplitButtonSize) -> f32 {
    size.single_width()
}

/// The value to request when the toggle is pressed.
pub fn next_local_request(state: &BandwidthModeState) -> bool {
    !state.local_requested
}

pub fn tooltip_text(state: &BandwidthModeState) -> String {
    if !state.active {
        return "Low bandwidth off".to_string();
    }
    let requesters: Vec<&str> = state
        .local_requested
        .then_some("you")
        .into_iter()
        .chain(state.requested_by.iter().map(String::as_str))
        .collect();
    format!("Low bandwidth: requested by {}", requesters.join(", "))
}

fn background(state: &BandwidthModeState) -> Color {
    match (state.active, state.local_requested) {
        (false, _) => ColorToken::Gray400.to_color(),
        (true, true) => ColorToken::Sky500.to_color(),
        // Active because a remote participant asked for it.
        (true, false) => ColorToken::Sky800.to_color(),
    }
}

pub fn bandwidth_toggle<'a, Message: Clone + 'a>(
    state: &BandwidthModeState,
    on_press: Message,
    size: SplitButtonSize,
) -> Element<'a, Message, Theme, iced::Renderer> {
    let btn = split_button_svg_sized(ICON_TURTLE, background(state), on_press, None, false, size);

    let tooltip_content = container(text(tooltip_text(state)).size(12).color(Color::WHITE))
        .padding(Padding::from([4.0, 8.0]))
        .style(|_theme: &Theme| container::Style {
            background: Some(Background::Color(ColorToken::Gray600.to_color())),
            border: Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                width: 1.0,
                radius: 6.0.into(),
            },
            shadow: ShadowToken::Xs.to_shadow(),
            ..Default::default()
        });

    tooltip(btn, tooltip_content, tooltip::Position::Bottom)
        .gap(4)
        .snap_within_viewport(true)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(active: bool, local_requested: bool, requested_by: &[&str]) -> BandwidthModeState {
        BandwidthModeState {
            active,
            local_requested,
            requested_by: requested_by.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn tooltip_off() {
        assert_eq!(tooltip_text(&state(false, false, &[])), "Low bandwidth off");
    }

    #[test]
    fn tooltip_local_only() {
        assert_eq!(
            tooltip_text(&state(true, true, &[])),
            "Low bandwidth: requested by you"
        );
    }

    #[test]
    fn tooltip_local_and_remote() {
        assert_eq!(
            tooltip_text(&state(true, true, &["Ann", "Bo"])),
            "Low bandwidth: requested by you, Ann, Bo"
        );
    }

    #[test]
    fn tooltip_remote_only() {
        assert_eq!(
            tooltip_text(&state(true, false, &["Ann"])),
            "Low bandwidth: requested by Ann"
        );
    }

    #[test]
    fn press_toggles_local_request() {
        assert!(next_local_request(&state(true, false, &["Ann"])));
        assert!(!next_local_request(&state(true, true, &[])));
    }
}
