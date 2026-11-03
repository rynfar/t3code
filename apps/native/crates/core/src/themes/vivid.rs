//! `themePalette.ts` `createVividThemeColors` for environment canvas/accent seeds.
use super::{Colors, color::*, defaults};
fn quantize(c: Color) -> Color {
    let n = packed(mapped(c, true));
    [f64::from(n >> 24) / 255., f64::from((n >> 16) & 255) / 255., f64::from((n >> 8) & 255) / 255., f64::from(n & 255) / 255.]
}
fn foreground(c: Color) -> Color {
    let light = [1., 250. / 255., 1., 1.];
    let dark = [36. / 255., 21. / 255., 35. / 255., 1.];
    let a = contrast(c, light);
    let b = contrast(c, dark);
    if a.max(b) >= 4.5 {
        if a >= b { light } else { dark }
    } else if contrast(c, [1.; 4]) >= contrast(c, [0., 0., 0., 1.]) {
        [1.; 4]
    } else {
        [0., 0., 0., 1.]
    }
}
fn readable(bg: Color, fg: Color, ratio: f64) -> Color {
    let mut result = fg;
    let (mut low, mut high) = (0., 1.);
    for _ in 0..12 {
        let amount = (low + high) / 2.;
        let c = quantize(mix(bg, fg, amount, "srgb"));
        if contrast(c, bg) >= ratio {
            result = c;
            low = amount;
        } else {
            high = amount;
        }
    }
    result
}
pub(super) fn derive(dark_appearance: bool, canvas: &str, accent: &str) -> Option<Colors> {
    let canvas = quantize(parse(canvas).ok()?);
    let accent_rgb = quantize(parse(accent).ok()?);
    let canvas_lch = lch(canvas);
    let accent = lch(accent_rgb);
    let dark = luminance(canvas) < 0.179;
    let hue = if accent[1] < 0.02 { canvas_lch[2] } else { accent[2] };
    let tint = (accent[1] * 0.22).clamp(0.008, 0.045);
    let step = if dark { 1. } else { -1. };
    let at = |delta: f64, chroma: f64| [(canvas_lch[0] + step * delta).clamp(0.05, 0.98), chroma, hue];
    let rgb = |c| quantize(from_lch(c));
    let base = [if dark { 0.95 } else { 0.2 }, (accent[1] * 0.25).min(0.035), hue];
    let text = quantize(solve(base, &[canvas], 7., dark));
    let on = |c: Color| solve(base, &[c], 4.6, dark);
    let muted_on = |c| readable(c, text, if luminance(c) < 0.179 { 5.082 } else { 4.705 });
    let action =
        [(accent[0] + if dark { 0.06 } else { -0.02 }).clamp(0.35, 0.85), (accent[1] * 0.9).max(0.06), (hue + 50.).rem_euclid(360.)];
    let action_rgb = rgb(action);
    let sidebar = at(0.045, tint * 1.4);
    let surface = at(0.015, tint);
    let raised = at(0.05, tint);
    let overlay = at(0.075, tint);
    let border = at(if dark { 0.16 } else { 0.12 }, (accent[1] * 0.35).min(0.07));
    let input = at(if dark { 0.21 } else { 0.16 }, (accent[1] * 0.4).min(0.08));
    let secondary = at(if dark { 0.1 } else { 0.06 }, (accent[1] * 0.5).min(0.09));
    let muted = at(if dark { 0.06 } else { 0.04 }, (accent[1] * 0.35).min(0.06));
    let accent_surface = at(if dark { 0.13 } else { 0.08 }, (accent[1] * 0.55).min(0.11));
    let message = at(if dark { 0.16 } else { 0.1 }, (accent[1] * 0.6).min(0.13));
    let code = at(0.035, tint * 0.8);
    let update = at(if dark { 0.14 } else { 0.09 }, (accent[1] * 0.55).min(0.12));
    let search = rgb(at(0.22, accent[1].min(0.12)));
    let active = rgb(at(0.4, accent[1].min(0.18)));
    let mut colors = defaults(dark_appearance);
    let mut put = |role: &str, c: Color| {
        colors.insert(role.into(), css(mapped(c, false)));
    };
    for (role, c) in [
        ("canvas", canvas),
        ("chrome", canvas),
        ("toolbar", canvas),
        ("text", text),
        ("toolbarForeground", text),
        ("toolbarControlForeground", text),
        ("codeForeground", text),
        ("terminalForeground", text),
        ("terminalBackground", canvas),
        ("focus", accent_rgb),
        ("accent", accent_rgb),
        ("update", accent_rgb),
        ("terminalCursor", accent_rgb),
        ("accentForeground", foreground(accent_rgb)),
        ("messageAction", action_rgb),
        ("messageActionForeground", foreground(action_rgb)),
        ("textMuted", muted_on(canvas)),
        ("secondaryLabel", muted_on(canvas)),
        ("iconMuted", muted_on(canvas)),
        ("mutedForeground", readable(rgb(muted), text, 4.6)),
        ("placeholder", readable(rgb(raised), text, 4.6)),
        ("secondaryForeground", on(rgb(secondary))),
        ("accentSurfaceForeground", on(rgb(accent_surface))),
        ("messageForeground", on(rgb(message))),
        ("updateForeground", on(rgb(update))),
        ("sidebarForeground", on(rgb(sidebar))),
        ("sidebarMutedForeground", muted_on(rgb(sidebar))),
        ("searchMatchBackground", search),
        ("searchMatchForeground", foreground(search)),
        ("searchMatchActiveBackground", active),
        ("searchMatchActiveForeground", foreground(active)),
    ] {
        put(role, c);
    }
    for (role, c) in [
        ("surface", surface),
        ("surfaceRaised", raised),
        ("surfaceOverlay", overlay),
        ("border", border),
        ("input", input),
        ("secondary", secondary),
        ("muted", muted),
        ("accentSurface", accent_surface),
        ("messageSurface", message),
        ("codeBackground", code),
        ("updateSurface", update),
        ("sidebar", sidebar),
        ("toolbarBorder", at(if dark { 0.14 } else { 0.1 }, (accent[1] * 0.4).min(0.08))),
        ("toolbarControl", at(if dark { 0.09 } else { 0.05 }, tint * 1.3)),
        ("toolbarControlHover", at(if dark { 0.14 } else { 0.09 }, tint * 1.6)),
        ("sidebarControlSurface", at(if dark { 0.1 } else { 0.07 }, tint * 1.5)),
        ("sidebarRowHover", at(if dark { 0.08 } else { 0.06 }, (accent[1] * 0.45).min(0.08))),
        ("sidebarRowActive", at(if dark { 0.12 } else { 0.09 }, (accent[1] * 0.55).min(0.1))),
        ("sidebarRowSelected", at(if dark { 0.14 } else { 0.1 }, (accent[1] * 0.6).min(0.11))),
        ("sidebarBorder", at(if dark { 0.17 } else { 0.12 }, (accent[1] * 0.4).min(0.08))),
        ("terminalSelection", at(if dark { 0.18 } else { 0.12 }, (accent[1] * 0.55).min(0.12))),
        ("terminalScrollbar", at(if dark { 0.22 } else { 0.16 }, tint)),
        ("terminalScrollbarHover", at(if dark { 0.3 } else { 0.22 }, tint)),
        ("messageActionHover", [action[0] + if dark { 0.06 } else { -0.06 }, action[1], action[2]]),
    ] {
        put(role, from_lch(c));
    }
    for (role, fg, surface) in [
        ("error", if dark { "#fb414a" } else { "#fb2c36" }, if dark { "#ff6467" } else { "#c10007" }),
        ("warning", "#fe9a00", if dark { "#ffb900" } else { "#bb4d00" }),
    ] {
        let error = parse(fg).ok()?;
        let bg = quantize(mix(canvas, error, if dark { 0.84 } else { 0.92 }, "srgb"));
        put(role, error);
        put(&format!("{role}Surface"), bg);
        put(&format!("{role}Foreground"), solve(lch(parse(surface).ok()?), &[bg], 4.6, dark));
    }
    Some(colors)
}
