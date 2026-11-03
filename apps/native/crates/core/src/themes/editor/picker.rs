//! HEX/RGB/HSV editing from `ThemeColorPicker.tsx` and `web/src/lib/color.ts`.
use super::super::color;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsv {
    pub h: f64,
    pub s: f64,
    pub v: f64,
}
pub fn hex(value: &str) -> Option<String> {
    let n = color::packed(color::mapped(color::parse(value).ok()?, true));
    Some(if n & 255 == 255 {
        format!("#{:06x}", n >> 8)
    } else {
        format!("#{n:08x}")
    })
}
pub fn swatch(value: &str) -> Option<u32> {
    hex(value).and_then(|s| color::parse(&s).ok()).map(color::packed)
}
pub fn rgb(value: &str) -> Option<String> {
    let n = swatch(value)?;
    Some(format!("{}, {}, {}", n >> 24, (n >> 16) & 255, (n >> 8) & 255))
}
pub fn from_rgb(draft: &str, previous: &str) -> Option<String> {
    let draft = draft.trim();
    let draft = if draft.to_lowercase().starts_with("rgb(") {
        draft[4..].trim_end_matches(')')
    } else {
        draft
    };
    let c: Vec<_> = draft
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::parse::<u8>)
        .collect::<Result<_, _>>()
        .ok()?;
    if c.len() != 3 {
        return None;
    }
    Some(with_alpha(&format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]), previous))
}
pub fn with_alpha(rgb: &str, previous: &str) -> String {
    let previous = hex(previous).unwrap_or_default();
    format!("{}{}", rgb.to_lowercase(), previous.get(7..).unwrap_or(""))
}
pub fn from_hex(draft: &str, previous: &str) -> Option<String> {
    (draft.len() == 7 && draft.starts_with('#') && draft[1..].bytes().all(|b| b.is_ascii_hexdigit())).then(|| with_alpha(draft, previous))
}
pub fn hsv(value: &str) -> Hsv {
    let n = swatch(value).unwrap_or(255);
    let [r, g, b] = [n >> 24, (n >> 16) & 255, (n >> 8) & 255].map(|c| f64::from(c) / 255.);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let h = if delta == 0. {
        0.
    } else if max == r {
        ((g - b) / delta).rem_euclid(6.)
    } else if max == g {
        (b - r) / delta + 2.
    } else {
        (r - g) / delta + 4.
    } * 60.;
    Hsv {
        h,
        s: if max == 0. { 0. } else { delta / max },
        v: max,
    }
}
pub fn from_hsv(hsv: Hsv, previous: &str) -> String {
    let h = hsv.h.rem_euclid(360.) / 60.;
    let c = hsv.v * hsv.s;
    let x = c * (1. - (h.rem_euclid(2.) - 1.).abs());
    let m = hsv.v - c;
    let [r, g, b] = if h < 1. {
        [c, x, 0.]
    } else if h < 2. {
        [x, c, 0.]
    } else if h < 3. {
        [0., c, x]
    } else if h < 4. {
        [0., x, c]
    } else if h < 5. {
        [x, 0., c]
    } else {
        [c, 0., x]
    };
    with_alpha(
        &format!(
            "#{:02x}{:02x}{:02x}",
            ((r + m) * 255.).round() as u8,
            ((g + m) * 255.).round() as u8,
            ((b + m) * 255.).round() as u8
        ),
        previous,
    )
}
#[derive(Clone, Copy)]
pub enum Axis {
    Saturation,
    Brightness,
    Hue,
}
pub fn key(mut hsv: Hsv, axis: Axis, key: &str, shift: bool) -> Option<Hsv> {
    let hue = matches!(axis, Axis::Hue);
    let step = if hue {
        if shift { 10. } else { 1. }
    } else if shift {
        0.1
    } else {
        0.02
    };
    let value = match axis {
        Axis::Saturation => &mut hsv.s,
        Axis::Brightness => &mut hsv.v,
        Axis::Hue => &mut hsv.h,
    };
    *value = match key {
        "home" => 0.,
        "end" => {
            if hue {
                360.
            } else {
                1.
            }
        }
        "right" | "up" => *value + step,
        "left" | "down" => *value - step,
        _ => return None,
    };
    if hue && !matches!(key, "home" | "end") {
        *value = value.rem_euclid(360.);
    } else {
        *value = value.clamp(0., if hue { 360. } else { 1. });
    }
    Some(hsv)
}
