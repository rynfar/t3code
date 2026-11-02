//! One theme source: gpui-component's `Theme` global. T3's palette (generated into `tokens.rs`
//! from the web theme) is written into it, so primitives and app views read the same tokens.

use gpui::{App, Window};
use gpui_component::Theme;

pub fn init(_cx: &mut App) {}

pub fn follow_system_appearance(window: &mut Window, cx: &mut App) {
    Theme::sync_system_appearance(Some(window), cx);
}
