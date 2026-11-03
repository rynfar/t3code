//! File conversion from `web/src/vscodeThemeImport.ts` (workbench colors, JSONC and alpha).
use super::{ThemeDefinition, color::*, decode, id_from_name, reserved, standard, vivid};
use serde_json::{Value, json};
fn hex(mut c: Color) -> String {
    c[3] = 1.;
    format!("#{:06x}", packed(c) >> 8)
}
fn parse_vscode(v: &Value) -> Option<Color> {
    let s = v.as_str()?.trim();
    if let Some(args) = s.strip_prefix("color(").and_then(|s| s.strip_suffix(')')) {
        let (space, body) = args.split_once(char::is_whitespace)?;
        let parts = split(body, '/');
        let n = |s: &str| {
            s.trim_end_matches('%').parse::<f64>().ok().filter(|v| v.is_finite()).map(|n| if s.ends_with('%') { n / 100. } else { n })
        };
        let values = parts[0].split_whitespace().map(n).collect::<Option<Vec<_>>>()?;
        let [r, g, b] = values.as_slice() else { return None };
        let a = if let Some(a) = parts.get(1) { n(a)?.clamp(0., 1.) } else { 1. };
        if space.eq_ignore_ascii_case("srgb") {
            return Some([*r, *g, *b, a]);
        }
        if !space.eq_ignore_ascii_case("display-p3") {
            return None;
        }
        let lin = |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        let gamma = |v: f64| {
            let v = v.clamp(0., 1.);
            if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1. / 2.4) - 0.055 }
        };
        let (r, g, b) = (lin(*r), lin(*g), lin(*b));
        return Some([
            gamma(1.2249401762805 * r - 0.2249401762805 * g),
            gamma(-0.042056961239 * r + 1.042056961239 * g),
            gamma(-0.0196375547643 * r - 0.0786360655012 * g + 1.0982736202656 * b),
            a,
        ]);
    }
    let s = s.trim_start_matches('#');
    if ![3, 4, 6, 8].contains(&s.len()) || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    parse(&format!("#{s}")).ok()
}
fn rgb(s: &str) -> Color {
    mapped(parse(s).unwrap_or([0., 0., 0., 1.]), true)
}
fn flatten(mut c: Color, base: Color) -> String {
    let alpha = c[3];
    c[3] = 1.;
    hex(mix(c, base, alpha, "srgb"))
}
fn pick(colors: &Value, keys: &[&str]) -> Option<Color> {
    keys.iter().find_map(|k| parse_vscode(&colors[*k]))
}
fn solid(colors: &Value, base: Color, keys: &[&str]) -> Option<String> {
    pick(colors, keys).map(|c| flatten(c, base))
}
fn apart(a: &str, b: &str) -> bool {
    contrast(rgb(a), rgb(b)) >= 1.1
}
fn readable(colors: &Value, surface: &str, fallback: &str, keys: &[&str]) -> String {
    let bg = rgb(surface);
    let specified = solid(colors, bg, keys);
    if let Some(c) = specified.filter(|c| contrast(rgb(c), bg) >= 4.5) {
        return c;
    }
    if contrast(rgb(fallback), bg) >= 4.5 {
        return fallback.into();
    }
    if luminance(bg) < 0.179 { "#ffffff".into() } else { "#000000".into() }
}
fn name(value: &Value) -> String {
    let s =
        ["displayName", "name"].iter().find_map(|k| value[*k].as_str().filter(|s| !s.trim().is_empty())).unwrap_or("VS Code theme").trim();
    let human = if !s.contains(char::is_whitespace) && s.contains(['-', '_', '.']) {
        s.split(['-', '_', '.'])
            .filter(|s| !s.is_empty())
            .map(|s| {
                let mut cs = s.chars();
                cs.next().unwrap().to_uppercase().chain(cs).collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        s.into()
    };
    human.chars().take(48).collect()
}
pub(super) fn convert(v: &Value) -> Result<ThemeDefinition, String> {
    let colors = &v["colors"];
    let mut canvas = pick(colors, &["editor.background", "editorPane.background"])
        .ok_or("That VS Code theme has no \"editor.background\" color, so there is nothing to build a palette from.")?;
    canvas[3] = 1.;
    let dark = match v["type"].as_str().map(str::to_lowercase).as_deref() {
        Some("light" | "hc-light") => false,
        Some("dark" | "hc-black") => true,
        _ => luminance(canvas) < 0.179,
    };
    let canvas_hex = hex(canvas);
    let raised = solid(colors, canvas, &["editorWidget.background", "dropdown.background"]);
    let stands = |s: &str| apart(s, &canvas_hex) && raised.as_ref().is_none_or(|r| apart(s, r));
    let (accent, accent_color) = [
        "focusBorder",
        "button.background",
        "textLink.foreground",
        "activityBarBadge.background",
        "progressBar.background",
        "badge.background",
    ]
    .into_iter()
    .filter_map(|key| pick(colors, &[key]).map(|c| (flatten(c, canvas), c)))
    .find(|(s, _)| stands(s))
    .unwrap_or_else(|| {
        let accent = hex(rgb(&standard(dark)["accent"]));
        let accent = [accent.clone(), "#ffffff".into(), "#000000".into()].into_iter().find(|s| stands(s)).unwrap_or(accent);
        let c = rgb(&accent);
        (accent, c)
    });
    let mut tint = accent_color;
    tint[3] = 0.2;
    let derived = vivid::derive(dark, &canvas_hex, &flatten(tint, canvas)).ok_or("Could not derive this theme.")?;
    let sidebar = solid(colors, canvas, &["sideBar.background", "activityBar.background"]).unwrap_or_else(|| derived["sidebar"].clone());
    let terminal =
        solid(colors, canvas, &["terminal.background", "panel.background"]).unwrap_or_else(|| derived["terminalBackground"].clone());
    let raised = raised.unwrap_or_else(|| derived["surfaceRaised"].clone());
    let action = solid(colors, canvas, &["button.background"])
        .filter(|s| apart(s, &canvas_hex) && apart(s, &raised))
        .unwrap_or_else(|| accent.clone());
    let mut input = [
        derived["input"].clone(),
        derived["surfaceRaised"].clone(),
        standard(dark)["input"].clone(),
        "#000000".into(),
        "#ffffff".into(),
        "#808080".into(),
    ]
    .into_iter()
    .find(|s| apart(s, &canvas_hex) && apart(s, &action))
    .unwrap_or_else(|| "#808080".into());
    if let Some(c) = ["input.background", "input.border"]
        .iter()
        .filter_map(|k| solid(colors, canvas, &[k]))
        .find(|s| apart(s, &canvas_hex) && apart(s, &action))
    {
        input = c;
    }
    let mut out = derived.clone();
    for (role, base, keys) in [
        ("surface", canvas, &["editorWidget.background"][..]),
        ("surfaceOverlay", canvas, &["menu.background", "quickInput.background", "dropdown.background"]),
        ("border", canvas, &["panel.border", "editorGroup.border", "contrastBorder"]),
        ("accentSurface", canvas, &["list.activeSelectionBackground", "list.hoverBackground"]),
        ("codeBackground", canvas, &["textCodeBlock.background"]),
        ("searchMatchBackground", canvas, &["editor.findMatchHighlightBackground"]),
        ("searchMatchActiveBackground", canvas, &["editor.findMatchBackground"]),
        ("sidebarBorder", rgb(&sidebar), &["sideBar.border"]),
        ("sidebarRowHover", rgb(&sidebar), &["list.hoverBackground"]),
        ("sidebarRowActive", rgb(&sidebar), &["list.inactiveSelectionBackground", "list.hoverBackground"]),
        ("sidebarRowSelected", rgb(&sidebar), &["list.activeSelectionBackground"]),
        ("terminalCursor", rgb(&terminal), &["terminalCursor.foreground", "editorCursor.foreground"]),
        ("terminalSelection", rgb(&terminal), &["terminal.selectionBackground", "editor.selectionBackground"]),
        ("terminalScrollbar", rgb(&terminal), &["scrollbarSlider.background"]),
    ] {
        if let Some(c) = solid(colors, base, keys) {
            out.insert(role.into(), c);
        }
    }
    for (role, surface, keys) in [
        ("text", canvas_hex.as_str(), &["editor.foreground", "foreground"][..]),
        ("textMuted", &canvas_hex, &["descriptionForeground", "disabledForeground"]),
        ("placeholder", &raised, &["input.placeholderForeground"]),
        ("sidebarForeground", &sidebar, &["sideBar.foreground"]),
        ("terminalForeground", &terminal, &["terminal.foreground"]),
        ("messageActionForeground", &action, &["button.foreground"]),
        ("accentForeground", &accent, &["button.foreground"]),
    ] {
        out.insert(role.into(), readable(colors, surface, &derived[role], keys));
    }
    for (role, surface, keys) in [
        ("searchMatchForeground", "searchMatchBackground", &["editor.findMatchHighlightForeground", "editor.foreground", "foreground"][..]),
        ("searchMatchActiveForeground", "searchMatchActiveBackground", &["editor.findMatchForeground", "editor.foreground", "foreground"]),
    ] {
        out.insert(role.into(), readable(colors, &out[surface], &derived[role], keys));
    }
    for (role, keys) in [("error", &["editorError.foreground", "errorForeground"][..]), ("warning", &["editorWarning.foreground"])] {
        let base = solid(colors, canvas, keys).filter(|s| apart(s, &canvas_hex)).unwrap_or_else(|| derived[role].clone());
        let c = parse(&base)?;
        let bg = rgb(&canvas_hex);
        let solved = if contrast(mapped(c, true), bg) >= 4.5 { c } else { solve(lch(c), &[bg], 4.5, luminance(bg) < 0.179) };
        out.insert(role.into(), css(mapped(solved, false)));
    }
    for (role, value) in [
        ("canvas", canvas_hex),
        ("surfaceRaised", raised),
        ("input", input),
        ("sidebar", sidebar),
        ("terminalBackground", terminal),
        ("accent", accent.clone()),
        ("focus", accent),
        ("messageAction", action),
    ] {
        out.insert(role.into(), value);
    }
    let name = name(v);
    let id = id_from_name(&name);
    let id = if reserved(&id) { format!("{id}-vscode") } else { id };
    decode(&json!({"version":1,"id":id,"name":name,"appearance":if dark{"dark"}else{"light"},"colors":out}), false)
}
