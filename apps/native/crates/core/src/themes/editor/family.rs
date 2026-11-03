//! Color families from `ThemeEditorPanel.tsx` / `themePalette.ts`.
use super::super::{
    Colors,
    color::*,
    vivid::{foreground, quantize},
};
pub struct Family {
    pub group: &'static str,
    pub label: &'static str,
    pub role: &'static str,
    pub roles: &'static [&'static str],
}
impl Family {
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        self.label.to_lowercase().contains(&q)
            || self.roles.iter().any(|r| {
                let mut label = String::new();
                for c in r.chars() {
                    if c.is_uppercase() {
                        label.push(' ');
                    }
                    label.extend(c.to_lowercase());
                }
                label.contains(&q)
            })
    }
}
pub const FAMILIES: &[Family] = &[
    Family {
        group: "Foundation",
        label: "Background",
        role: "canvas",
        roles: &["canvas", "chrome", "toolbar"],
    },
    Family {
        group: "Foundation",
        label: "Surface",
        role: "surface",
        roles: &["surface"],
    },
    Family {
        group: "Foundation",
        label: "Raised surface",
        role: "surfaceRaised",
        roles: &["surfaceRaised"],
    },
    Family {
        group: "Foundation",
        label: "Overlay",
        role: "surfaceOverlay",
        roles: &["surfaceOverlay"],
    },
    Family {
        group: "Foundation",
        label: "Text",
        role: "text",
        roles: &["text", "toolbarForeground", "toolbarControlForeground"],
    },
    Family {
        group: "Foundation",
        label: "Muted text",
        role: "mutedForeground",
        roles: &[
            "textMuted",
            "mutedForeground",
            "placeholder",
            "secondaryLabel",
            "iconMuted",
            "sidebarMutedForeground",
        ],
    },
    Family {
        group: "Foundation",
        label: "Border",
        role: "border",
        roles: &["border", "toolbarBorder", "sidebarBorder"],
    },
    Family {
        group: "Foundation",
        label: "Input",
        role: "input",
        roles: &["input"],
    },
    Family {
        group: "Brand & content",
        label: "Subtle surface",
        role: "secondary",
        roles: &["secondary", "secondaryForeground", "muted", "toolbarControl"],
    },
    Family {
        group: "Brand & content",
        label: "Highlight surface",
        role: "accentSurface",
        roles: &["accentSurface", "accentSurfaceForeground", "toolbarControlHover"],
    },
    Family {
        group: "Brand & content",
        label: "Accent",
        role: "accent",
        roles: &[
            "accent",
            "accentForeground",
            "focus",
            "update",
            "updateForeground",
            "updateSurface",
            "terminalCursor",
        ],
    },
    Family {
        group: "Brand & content",
        label: "Action",
        role: "messageAction",
        roles: &["messageAction", "messageActionForeground", "messageActionHover"],
    },
    Family {
        group: "Brand & content",
        label: "Message surface",
        role: "messageSurface",
        roles: &["messageSurface", "messageForeground"],
    },
    Family {
        group: "Brand & content",
        label: "Code surface",
        role: "codeBackground",
        roles: &["codeBackground", "codeForeground"],
    },
    Family {
        group: "Search",
        label: "Search matches",
        role: "searchMatchBackground",
        roles: &["searchMatchBackground", "searchMatchForeground"],
    },
    Family {
        group: "Search",
        label: "Current match",
        role: "searchMatchActiveBackground",
        roles: &["searchMatchActiveBackground", "searchMatchActiveForeground"],
    },
    Family {
        group: "Context",
        label: "Sidebar background",
        role: "sidebar",
        roles: &["sidebar", "sidebarForeground"],
    },
    Family {
        group: "Context",
        label: "Sidebar controls",
        role: "sidebarControlSurface",
        roles: &["sidebarControlSurface"],
    },
    Family {
        group: "Context",
        label: "Sidebar selection",
        role: "sidebarRowSelected",
        roles: &["sidebarRowHover", "sidebarRowActive", "sidebarRowSelected"],
    },
    Family {
        group: "Context",
        label: "Terminal background",
        role: "terminalBackground",
        roles: &[
            "terminalBackground",
            "terminalForeground",
            "terminalSelection",
            "terminalScrollbar",
            "terminalScrollbarHover",
        ],
    },
    Family {
        group: "Status",
        label: "Error",
        role: "error",
        roles: &["error", "errorForeground", "errorSurface"],
    },
    Family {
        group: "Status",
        label: "Warning",
        role: "warning",
        roles: &["warning", "warningForeground", "warningSurface"],
    },
];
pub fn update_family(dark: bool, colors: &Colors, role: &str, value: &str) -> Colors {
    let mut next = colors.clone();
    let Ok(parsed) = parse(value) else {
        next.insert(role.into(), value.into());
        return next;
    };
    let normalized = canonical(parsed);
    let mut selected = quantize(parsed);
    selected[3] = 1.;
    let canvas = quantize(parse(&colors["canvas"]).unwrap_or(if dark {
        [24. / 255., 15. / 255., 27. / 255., 1.]
    } else {
        [250. / 255., 245. / 255., 250. / 255., 1.]
    }));
    let on = |bg| mix(selected, bg, parsed[3], "srgb");
    let painted = on(canvas);
    let canvas_dark = luminance(canvas) < 0.179;
    let mut put = |key: &str, c: Color| {
        next.insert(key.into(), canonical(c));
    };
    let fg = |bg| foreground(bg);
    let tone = |bg| solve(lch(parsed), &[bg], 4.6, luminance(bg) < 0.179);
    match role {
        "secondary" => {
            put("secondaryForeground", fg(painted));
        }
        "accentSurface" => {
            put("accentSurfaceForeground", fg(painted));
        }
        "accent" => {
            let surface = mix(painted, canvas, if canvas_dark { 0.32 } else { 0.16 }, "srgb");
            put("accentForeground", fg(painted));
            put("updateForeground", tone(surface));
            put("updateSurface", surface);
        }
        "messageAction" => {
            let foreground = fg(painted);
            let opposite = if foreground[0] > 0.9 { [0., 0., 0., 1.] } else { [1.; 4] };
            let mut hover = mix(selected, opposite, 0.88, "srgb");
            hover[3] = parsed[3];
            put("messageActionForeground", foreground);
            put("messageActionHover", hover);
        }
        "messageSurface" => put("messageForeground", fg(painted)),
        "codeBackground" => put("codeForeground", fg(painted)),
        "sidebar" => put("sidebarForeground", fg(painted)),
        "searchMatchBackground" => put("searchMatchForeground", fg(painted)),
        "searchMatchActiveBackground" => put("searchMatchActiveForeground", fg(painted)),
        "sidebarRowSelected" => {
            let sidebar = quantize(parse(&colors["sidebar"]).unwrap_or(canvas));
            let painted = on(sidebar);
            put("sidebarRowHover", mix(painted, sidebar, 0.5, "srgb"));
            put("sidebarRowActive", mix(painted, sidebar, 0.8, "srgb"));
        }
        "terminalBackground" => {
            let fg = fg(painted);
            let dark = luminance(painted) < 0.179;
            put("terminalForeground", fg);
            put(
                "terminalSelection",
                mix(
                    quantize(parse(&colors["accent"]).unwrap_or([168. / 255., 67. / 255., 112. / 255., 1.])),
                    painted,
                    if dark { 0.35 } else { 0.18 },
                    "srgb",
                ),
            );
            put("terminalScrollbar", mix(fg, painted, if dark { 0.42 } else { 0.22 }, "srgb"));
            put("terminalScrollbarHover", mix(fg, painted, if dark { 0.55 } else { 0.32 }, "srgb"));
        }
        "error" | "warning" => {
            let surface = mix(painted, canvas, if canvas_dark { 0.16 } else { 0.08 }, "srgb");
            put(&format!("{role}Foreground"), tone(surface));
            put(&format!("{role}Surface"), surface);
        }
        _ => {}
    }
    let linked: &[&str] = match role {
        "canvas" => &["canvas", "chrome", "toolbar"],
        "text" => &["text", "toolbarForeground", "toolbarControlForeground"],
        "mutedForeground" => &[
            "textMuted",
            "mutedForeground",
            "placeholder",
            "secondaryLabel",
            "iconMuted",
            "sidebarMutedForeground",
        ],
        "border" => &["border", "toolbarBorder", "sidebarBorder"],
        "secondary" => &["secondary", "muted", "toolbarControl"],
        "accentSurface" => &["accentSurface", "toolbarControlHover"],
        "accent" => &["accent", "focus", "update", "terminalCursor"],
        _ => &[],
    };
    for r in linked {
        next.insert((*r).into(), normalized.clone());
    }
    next.insert(
        role.into(),
        if matches!(role, "searchMatchBackground" | "searchMatchActiveBackground") {
            canonical(painted)
        } else {
            normalized
        },
    );
    next
}

fn canonical(c: Color) -> String {
    let [l, chroma, h] = lch(c);
    let number = |v: f64, p: usize| {
        let s = format!("{:.*}", p, if v.abs() < 10f64.powi(-(p as i32)) / 2. { 0. } else { v });
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    };
    let body = format!(
        "{} {} {}",
        number(l, 6),
        number(chroma, 6),
        number(if chroma < 0.0000005 { 0. } else { h }, 3)
    );
    if c[3] < 1. {
        format!("oklch({body} / {})", number(c[3], 4))
    } else {
        format!("oklch({body})")
    }
}
