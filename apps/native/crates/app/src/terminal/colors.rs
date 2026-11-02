//! Terminal colors: background, foreground, cursor and selection come from the `terminal_*`
//! tokens; the 16 ANSI colors are the web's `TERMINAL_PALETTES` in
//! `apps/web/src/components/ThreadTerminalDrawer.tsx`, which are not themeable tokens.

use gpui::{App, Hsla, Rgba};
use gpui_component::ActiveTheme;

use t3_core::terminal::emulator::{Rgb, ThemeColors};
use crate::theme::{hsla, palette};

const LIGHT_ANSI: [u32; 16] = [
    0x000000, 0xcd3131, 0x107c10, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555, 0x666666, 0xcd3131, 0x14ce14,
    0xb5ba00, 0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5,
];

const DARK_ANSI: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666, 0xf14c4c, 0x23d18b,
    0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5,
];

fn rgb_of_hex(hex: u32) -> Rgb {
    Rgb { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8 }
}

/// Token colors are `0xRRGGBBAA`; the terminal paints them opaque.
fn rgb_of_token(token: u32) -> Rgb {
    rgb_of_hex(token >> 8)
}

pub fn theme_colors(cx: &App) -> ThemeColors {
    let p = palette(cx);
    let table = if cx.theme().is_dark() { &DARK_ANSI } else { &LIGHT_ANSI };
    ThemeColors {
        foreground: rgb_of_token(p.terminal_foreground),
        background: rgb_of_token(p.terminal_background),
        cursor: rgb_of_token(p.terminal_cursor),
        ansi: table.map(rgb_of_hex),
    }
}

pub fn to_hsla(rgb: Rgb) -> Hsla {
    Rgba { r: rgb.r as f32 / 255., g: rgb.g as f32 / 255., b: rgb.b as f32 / 255., a: 1. }.into()
}

pub struct Paint {
    pub background: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
    pub scrollbar: Hsla,
}

pub fn paint(cx: &App) -> Paint {
    let p = palette(cx);
    Paint {
        background: hsla(p.terminal_background),
        cursor: hsla(p.terminal_cursor),
        selection: hsla(p.terminal_selection),
        scrollbar: hsla(p.scrollbar_thumb_hover),
    }
}
