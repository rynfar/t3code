//! Full Lucide 0.564.0 icon catalog and SVG envelope builder for project icons.
//! Ports `apps/web/src/lib/lucideIconByName.ts` (all 1,671 canonical icons + deprecated aliases).
//! Baked with the design system §8 default stroke width (`1.75`).

use std::collections::HashMap;
use std::sync::LazyLock;

const CATALOG_TSV: &str = include_str!("lucide_catalog.tsv");

struct Catalog {
    canonical_names: Vec<&'static str>,
    bodies: HashMap<&'static str, &'static str>,
    aliases: HashMap<&'static str, &'static str>,
}

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    let mut canonical_names = Vec::with_capacity(1700);
    let mut bodies = HashMap::with_capacity(1700);
    let mut aliases = HashMap::with_capacity(260);
    for line in CATALOG_TSV.lines() {
        let Some((name, payload)) = line.split_once('\t') else {
            continue;
        };
        if let Some(target) = payload.strip_prefix('@') {
            aliases.insert(name, target);
        } else {
            canonical_names.push(name);
            bodies.insert(name, payload);
        }
    }
    Catalog { canonical_names, bodies, aliases }
});

/// All 1,671 canonical Lucide icon names in kebab-case (`lucideIconNames` in `lucideIconByName.ts`).
pub fn all_names() -> &'static [&'static str] {
    &CATALOG.canonical_names
}

/// Resolves a canonical icon name or deprecated alias (e.g. `"code-2"` -> `"code-xml"`,
/// `"alert-circle"` -> `"circle-alert"`) to its canonical kebab-case name.
pub fn resolve_canonical(name: &str) -> Option<&'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let catalog = &*CATALOG;
    if let Some((&canonical, _)) = catalog.bodies.get_key_value(trimmed) {
        return Some(canonical);
    }
    if let Some(&target) = catalog.aliases.get(trimmed)
        && let Some((&canonical, _)) = catalog.bodies.get_key_value(target)
    {
        return Some(canonical);
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some((&canonical, _)) = catalog.bodies.get_key_value(lower.as_str()) {
        return Some(canonical);
    }
    if let Some(&target) = catalog.aliases.get(lower.as_str())
        && let Some((&canonical, _)) = catalog.bodies.get_key_value(target)
    {
        return Some(canonical);
    }
    None
}

/// Returns true when `name` (canonical or alias) exists in the shipped Lucide catalog.
pub fn contains(name: &str) -> bool {
    resolve_canonical(name).is_some()
}

/// Returns the inner SVG markup for `name` (resolving aliases), or `None` for unknown icons.
pub fn svg_body(name: &str) -> Option<&'static str> {
    let canonical = resolve_canonical(name)?;
    CATALOG.bodies.get(canonical).copied()
}

/// Builds a standalone 24×24 Lucide SVG document with design-system §8 stroke (1.75) colored
/// with `stroke_hex` (e.g. `"#3b82f6"`) and `stroke_opacity` in `0.0..=1.0`.
pub fn render_svg(name: &str, stroke_hex: &str, stroke_opacity: f32) -> Option<Vec<u8>> {
    let body = svg_body(name)?;
    let opacity = stroke_opacity.clamp(0.0, 1.0);
    let mut out = String::with_capacity(body.len() + 220);
    out.push_str(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke=""#,
    );
    out.push_str(stroke_hex);
    out.push('"');
    if (opacity - 1.0).abs() > 0.001 {
        use std::fmt::Write;
        let _ = write!(out, r#" stroke-opacity="{opacity:.3}""#);
    }
    out.push_str(r#" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">"#);
    out.push_str(body);
    out.push_str("</svg>");
    Some(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ships_full_lucide_catalog_and_resolves_popular_and_deprecated_aliases() {
        assert!(all_names().len() >= 1600, "expected full Lucide 0.564.0 catalog, got {}", all_names().len());
        // Every popular icon from ProjectIconPickerDialog resolves (including aliases).
        for popular in [
            "folder-code",
            "code-2",
            "terminal",
            "globe-2",
            "server",
            "database",
            "bot",
            "sparkles",
            "smartphone",
            "monitor",
            "cloud-cog",
            "package",
            "book-open",
            "flask-conical",
            "shield-check",
            "rocket",
            "gamepad-2",
            "music",
            "image",
            "shopping-bag",
            "git-branch",
            "workflow",
            "wrench",
            "layers-3",
        ] {
            assert!(contains(popular), "popular icon {popular:?} must resolve");
            let svg = render_svg(popular, "#3b82f6", 1.0).expect("valid SVG");
            let text = std::str::from_utf8(&svg).unwrap();
            assert!(text.starts_with("<svg ") && text.ends_with("</svg>"));
            assert!(text.contains(r##"stroke="#3b82f6""##));
            assert!(text.contains(r#"stroke-width="1.75""#));
        }

        // Deprecated aliases resolve to canonical names.
        assert_eq!(resolve_canonical("code-2"), Some("code-xml"));
        assert_eq!(resolve_canonical("globe-2"), Some("earth"));
        assert_eq!(resolve_canonical("layers-3"), Some("layers"));
        assert_eq!(resolve_canonical("alert-circle"), Some("circle-alert"));
        assert_eq!(resolve_canonical("future-unknown-icon"), None);
        assert!(render_svg("future-unknown-icon", "#000000", 1.0).is_none());
    }
}
