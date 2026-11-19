//! Theme CSS sanitization, custom-property evaluation, and editor indentation helpers from
//! `packages/shared/src/themeCss.ts`, `apps/web/src/themeCss.ts`, `apps/web/src/index.css`,
//! and `apps/web/src/components/settings/ThemeCssEditor.tsx`.
//!
//! Arbitrary DOM selectors (such as `svg.lucide` or `[data-slot="button"]`) have no GPUI
//! equivalent and are counted as unmapped DOM rules while semantic/role CSS custom properties
//! apply live to the native palette.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use super::{Colors, color};

/// `THEME_CSS_MAX_LENGTH` from `packages/contracts/src/server.ts`.
pub const MAX_CSS_LEN: usize = 100_000;

/// Checks `css.length <= THEME_CSS_MAX_LENGTH` in UTF-16 code units, matching JavaScript.
pub fn is_valid_length(css: &str) -> bool {
    css.encode_utf16().count() <= MAX_CSS_LEN
}

/// Strips `/* ... */` block comments from CSS source.
pub fn strip_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            out.push(b as char);
            if b == b'\\' && i + 1 < bytes.len() {
                i += 1;
                out.push(bytes[i] as char);
            } else if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            quote = Some(b);
            out.push(b as char);
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        // Preserve multi-byte UTF-8 characters safely.
        let ch = css[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn contains_ci_ascii(haystack: &str, needle_lower: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle_lower.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return false;
    }
    h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

fn contains_src_fn(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 4 {
        return false;
    }
    for i in 0..=bytes.len() - 4 {
        if bytes[i..i + 4].eq_ignore_ascii_case(b"src(")
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
        {
            return true;
        }
    }
    false
}

/// Only CSS whitespace (`[ \t\n\f\r]`) may precede the URL scheme; U+00A0 makes it relative.
fn is_css_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0c | b'\r')
}

fn contains_non_local_url(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 4 {
        return false;
    }
    for i in 0..=bytes.len() - 4 {
        if !bytes[i..i + 4].eq_ignore_ascii_case(b"url(") {
            continue;
        }
        let mut j = i + 4;
        while j < bytes.len() && is_css_ws(bytes[j]) {
            j += 1;
        }
        if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
            j += 1;
        }
        let is_local = (j < bytes.len() && bytes[j] == b'#')
            || (j + 5 <= bytes.len() && bytes[j..j + 5].eq_ignore_ascii_case(b"data:"));
        if !is_local {
            return true;
        }
    }
    false
}

/// `themeCssValueFetches` from `packages/shared/src/themeCss.ts`.
///
/// Refuses any backslash (CSS escapes could hide `url` or `var`), `image-set(`, `\bsrc(`,
/// and any `url(` whose target does not begin with `data:` or `#`.
pub fn value_fetches(value: &str) -> bool {
    value.contains('\\')
        || contains_ci_ascii(value, "image-set(")
        || contains_src_fn(value)
        || contains_non_local_url(value)
}

/// `themeCssLoadsRemoteFiles` from `packages/shared/src/themeCss.ts`.
pub fn loads_remote_files(css: &str) -> bool {
    let code = strip_comments(css);
    contains_ci_ascii(&code, "@import")
        || contains_ci_ascii(&code, "image-set(")
        || contains_src_fn(&code)
        || contains_non_local_url(&code)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppearanceScope {
    Any,
    Light,
    Dark,
}

impl AppearanceScope {
    fn matches(self, dark: bool) -> bool {
        match self {
            Self::Any => true,
            Self::Light => !dark,
            Self::Dark => dark,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetScope {
    Root,
    Sidebar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CustomDecl {
    scope: TargetScope,
    appearance: AppearanceScope,
    name: String,
    value: String,
    important: bool,
    order: usize,
}

/// Summary of what a theme's CSS contains for editor feedback and diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CssDiagnostics {
    pub loads_remote_files: bool,
    pub exceeds_max_length: bool,
    pub applied_variables: usize,
    pub unmapped_dom_rules: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCss {
    declarations: Vec<CustomDecl>,
    diagnostics: CssDiagnostics,
}

impl ParsedCss {
    pub fn parse(css: &str) -> Self {
        let exceeds_max_length = !is_valid_length(css);
        let remote = loads_remote_files(css);
        let stripped = strip_comments(css);
        let mut parser = Parser {
            bytes: stripped.as_bytes(),
            source: &stripped,
            pos: 0,
            order: 0,
            declarations: Vec::new(),
            unmapped_dom_rules: 0,
            recognized_vars: BTreeSet::new(),
        };
        parser.parse_block(&[]);
        Self {
            declarations: parser.declarations,
            diagnostics: CssDiagnostics {
                loads_remote_files: remote,
                exceeds_max_length,
                applied_variables: parser.recognized_vars.len(),
                unmapped_dom_rules: parser.unmapped_dom_rules,
            },
        }
    }

    pub fn diagnostics(&self) -> CssDiagnostics {
        self.diagnostics
    }

    /// Collects the winning value per custom property for the requested appearance and scope.
    fn collect_vars(&self, dark: bool, scope: TargetScope) -> BTreeMap<String, String> {
        let mut best: BTreeMap<&str, (bool, usize, &str)> = BTreeMap::new();
        for decl in &self.declarations {
            if decl.scope != scope || !decl.appearance.matches(dark) {
                continue;
            }
            let replace = match best.get(decl.name.as_str()) {
                None => true,
                Some((imp, ord, _)) => {
                    (decl.important && !*imp) || (decl.important == *imp && decl.order >= *ord)
                }
            };
            if replace {
                best.insert(decl.name.as_str(), (decl.important, decl.order, decl.value.as_str()));
            }
        }
        best.into_iter().map(|(k, (_, _, v))| (k.to_owned(), v.to_owned())).collect()
    }

    /// Role overrides (`--app-theme-<kebab-role>` and direct role aliases like `--canvas`)
    /// evaluated into literal CSS colors for `dark`, preserving CSS cascade order & `!important`.
    pub fn role_overrides(&self, dark: bool, base_roles: &Colors) -> Colors {
        let vars = self.collect_vars(dark, TargetScope::Root);
        let mut best: BTreeMap<&'static str, (bool, usize, String)> = BTreeMap::new();
        for decl in &self.declarations {
            if decl.scope != TargetScope::Root || !decl.appearance.matches(dark) {
                continue;
            }
            let Some(role) = custom_prop_to_role(&decl.name) else {
                continue;
            };
            let resolved = resolve_var_expr(&decl.value, &vars, &mut BTreeSet::new());
            let Ok(c) = color::evaluate(&resolved, base_roles, dark) else {
                continue;
            };
            let replace = match best.get(role) {
                None => true,
                Some((imp, ord, _)) => {
                    (decl.important && !*imp) || (decl.important == *imp && decl.order >= *ord)
                }
            };
            if replace {
                best.insert(role, (decl.important, decl.order, color::css(c)));
            }
        }
        best.into_iter().map(|(role, (_, _, value))| (role.to_owned(), value)).collect()
    }

    /// Semantic palette token overrides (`--background`, `--foreground`, `--primary`, `--sidebar`,
    /// `--border`, etc.) evaluated into packed `0xRRGGBBAA` values for `dark`, preserving cascade order.
    pub fn token_overrides(&self, dark: bool, roles: &Colors) -> BTreeMap<String, u32> {
        let root_vars = self.collect_vars(dark, TargetScope::Root);
        let sidebar_vars = self.collect_vars(dark, TargetScope::Sidebar);
        let mut merged_sidebar = root_vars.clone();
        merged_sidebar.extend(sidebar_vars);

        let mut best: BTreeMap<&'static str, (bool, usize, u32)> = BTreeMap::new();
        for decl in &self.declarations {
            if !decl.appearance.matches(dark) {
                continue;
            }
            let (tokens, vars) = match decl.scope {
                TargetScope::Root => (root_prop_to_tokens(&decl.name), &root_vars),
                TargetScope::Sidebar => (sidebar_prop_to_tokens(&decl.name), &merged_sidebar),
            };
            if tokens.is_empty() {
                continue;
            }
            let resolved = resolve_var_expr(&decl.value, vars, &mut BTreeSet::new());
            let Ok(c) = color::evaluate(&resolved, roles, dark) else {
                continue;
            };
            let packed = color::packed(c);
            for token in tokens {
                let replace = match best.get(token) {
                    None => true,
                    Some((imp, ord, _)) => {
                        (decl.important && !*imp) || (decl.important == *imp && decl.order >= *ord)
                    }
                };
                if replace {
                    best.insert(*token, (decl.important, decl.order, packed));
                }
            }
        }

        best.into_iter().map(|(token, (_, _, packed))| (token.to_owned(), packed)).collect()
    }
}

/// Evaluates `css` diagnostics directly from a raw CSS string.
pub fn summarize_diagnostics(css: &str) -> CssDiagnostics {
    ParsedCss::parse(css).diagnostics()
}

struct Parser<'a> {
    bytes: &'a [u8],
    source: &'a str,
    pos: usize,
    order: usize,
    declarations: Vec<CustomDecl>,
    unmapped_dom_rules: usize,
    recognized_vars: BTreeSet<String>,
}

impl<'a> Parser<'a> {
    fn parse_block(&mut self, selectors: &[String]) {
        let mut buf = String::new();
        let mut depth: usize = 0;
        let mut quote: Option<u8> = None;

        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if let Some(q) = quote {
                buf.push(b as char);
                self.pos += 1;
                if b == b'\\' && self.pos < self.bytes.len() {
                    buf.push(self.bytes[self.pos] as char);
                    self.pos += 1;
                } else if b == q {
                    quote = None;
                }
                continue;
            }
            if b == b'"' || b == b'\'' {
                quote = Some(b);
                buf.push(b as char);
                self.pos += 1;
                continue;
            }
            if b == b'(' {
                depth += 1;
                buf.push('(');
                self.pos += 1;
                continue;
            }
            if b == b')' {
                depth = depth.saturating_sub(1);
                buf.push(')');
                self.pos += 1;
                continue;
            }
            if depth == 0 && b == b'{' {
                let prelude = squash_ws(&buf);
                buf.clear();
                self.pos += 1;
                self.handle_nested_block(selectors, &prelude);
                continue;
            }
            if depth == 0 && b == b'}' {
                self.flush_decl(selectors, &buf);
                buf.clear();
                self.pos += 1;
                return;
            }
            if depth == 0 && b == b';' {
                self.flush_decl(selectors, &buf);
                buf.clear();
                self.pos += 1;
                continue;
            }
            let ch = self.source[self.pos..].chars().next().unwrap();
            buf.push(ch);
            self.pos += ch.len_utf8();
        }
        self.flush_decl(selectors, &buf);
    }

    fn handle_nested_block(&mut self, parent_selectors: &[String], prelude: &str) {
        let lower = prelude.to_ascii_lowercase();
        if lower.starts_with("@keyframes")
            || lower.starts_with("@-webkit-keyframes")
            || lower.starts_with("@font-face")
            || lower.starts_with("@property")
        {
            // Skip block content safely; @keyframes/@font-face/@property are DOM-only.
            self.skip_block();
            self.unmapped_dom_rules += 1;
            return;
        }
        if lower.starts_with('@') {
            // @media / @supports / @layer: recurse preserving parent selectors (or @media prefers-color-scheme).
            let mut next = parent_selectors.to_vec();
            if lower.contains("prefers-color-scheme") {
                if lower.contains("dark") {
                    next.push(".dark".to_owned());
                } else if lower.contains("light") {
                    next.push(":root:not(.dark)".to_owned());
                }
            }
            self.parse_block(&next);
            return;
        }
        let mut next = parent_selectors.to_vec();
        next.push(prelude.to_owned());
        self.parse_block(&next);
    }

    fn skip_block(&mut self) {
        let mut depth: usize = 1;
        let mut quote: Option<u8> = None;
        while self.pos < self.bytes.len() && depth > 0 {
            let b = self.bytes[self.pos];
            self.pos += 1;
            if let Some(q) = quote {
                if b == b'\\' && self.pos < self.bytes.len() {
                    self.pos += 1;
                } else if b == q {
                    quote = None;
                }
                continue;
            }
            match b {
                b'"' | b'\'' => quote = Some(b),
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
        }
    }

    fn flush_decl(&mut self, selectors: &[String], raw: &str) {
        let s = raw.trim();
        if s.is_empty() || s.starts_with('@') {
            return;
        }
        let Some((prop_raw, val_raw)) = s.split_once(':') else {
            return;
        };
        let prop = prop_raw.trim();
        if prop.is_empty() {
            return;
        }
        let (val, important) = strip_important(val_raw.trim());
        if val.is_empty() || value_fetches(val) {
            return;
        }
        if !prop.starts_with("--") {
            self.unmapped_dom_rules += 1;
            return;
        }
        let targets = classify_selectors(selectors);
        if targets.is_empty() {
            self.unmapped_dom_rules += 1;
            return;
        }
        let recognized = custom_prop_to_role(prop).is_some()
            || !root_prop_to_tokens(prop).is_empty()
            || !sidebar_prop_to_tokens(prop).is_empty();
        if recognized {
            self.recognized_vars.insert(prop.to_owned());
        }
        for (scope, appearance) in targets {
            self.order += 1;
            self.declarations.push(CustomDecl {
                scope,
                appearance,
                name: prop.to_owned(),
                value: val.to_owned(),
                important,
                order: self.order,
            });
        }
    }
}

fn squash_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_important(val: &str) -> (&str, bool) {
    let trimmed = val.trim_end();
    if trimmed.len() >= 10
        && trimmed[trimmed.len() - 10..].eq_ignore_ascii_case("!important")
    {
        (trimmed[..trimmed.len() - 10].trim_end(), true)
    } else if let Some(idx) = trimmed.rfind('!')
        && trimmed[idx + 1..].trim().eq_ignore_ascii_case("important")
    {
        (trimmed[..idx].trim_end(), true)
    } else {
        (trimmed, false)
    }
}

fn classify_selectors(selectors: &[String]) -> Vec<(TargetScope, AppearanceScope)> {
    if selectors.is_empty() {
        // Bare custom properties without selector block: treat as root any-appearance.
        return vec![(TargetScope::Root, AppearanceScope::Any)];
    }
    let mut results = Vec::new();
    // Combine outer context (like @media dark) with innermost selector list.
    let mut outer_appearance = AppearanceScope::Any;
    for sel in &selectors[..selectors.len() - 1] {
        if let Some((_, app)) = classify_single_selector(sel)
            && app != AppearanceScope::Any
        {
            outer_appearance = app;
        }
    }
    let leaf = &selectors[selectors.len() - 1];
    for part in split_selector_list(leaf) {
        if let Some((scope, app)) = classify_single_selector(part) {
            let combined = match (outer_appearance, app) {
                (AppearanceScope::Any, a) => a,
                (a, AppearanceScope::Any) => a,
                (a, b) if a == b => a,
                _ => continue,
            };
            if !results.contains(&(scope, combined)) {
                results.push((scope, combined));
            }
        }
    }
    results
}

fn split_selector_list(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut bracket = 0usize;
    let mut start = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            ',' if depth == 0 && bracket == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

fn classify_single_selector(selector: &str) -> Option<(TargetScope, AppearanceScope)> {
    let lower = selector.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    let is_sidebar = lower.contains("[data-app-sidebar]")
        || lower.contains("[data-slot=\"sidebar\"]")
        || lower.contains("[data-slot='sidebar']")
        || lower.contains("[data-slot=\"sidebar-inner\"]")
        || lower.contains("[data-slot='sidebar-inner']");

    let is_light = lower.contains(":not(.dark)")
        || lower == ".light"
        || lower.ends_with(".light")
        || lower.contains(".light ")
        || lower.contains("[data-theme=\"light\"]")
        || lower.contains("[data-theme='light']");
    let is_dark = !is_light
        && (lower == ".dark"
            || lower.contains(".dark ")
            || lower.contains(".dark[")
            || lower.contains(".dark:")
            || lower.ends_with(".dark")
            || lower.contains("[data-theme=\"dark\"]")
            || lower.contains("[data-theme='dark']"));

    let appearance = if is_light {
        AppearanceScope::Light
    } else if is_dark {
        AppearanceScope::Dark
    } else {
        AppearanceScope::Any
    };

    if is_sidebar {
        return Some((TargetScope::Sidebar, appearance));
    }

    // Check if selector targets the root/document rather than a specific DOM widget.
    let stripped = lower
        .replace(":root", "")
        .replace("html", "")
        .replace("body", "")
        .replace(":not(.dark)", "")
        .replace(":not([data-theme-id=\"\"])", "")
        .replace("[data-theme-id]", "")
        .replace(".dark", "")
        .replace(".light", "")
        .replace('*', "");
    let remaining = stripped.trim();
    if remaining.is_empty()
        || (remaining.starts_with("[data-theme-id=") && remaining.ends_with(']'))
    {
        return Some((TargetScope::Root, appearance));
    }

    None
}

fn resolve_var_expr(expr: &str, vars: &BTreeMap<String, String>, visiting: &mut BTreeSet<String>) -> String {
    let mut out = String::with_capacity(expr.len());
    let mut i = 0;
    let bytes = expr.as_bytes();
    while i < bytes.len() {
        if i + 4 <= bytes.len()
            && bytes[i..i + 4].eq_ignore_ascii_case(b"var(")
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
            && let Some((end, inner)) = take_balanced_parens(expr, i + 4)
        {
            let parts = color::split(inner, ',');
            let name = parts.first().copied().unwrap_or("").trim();
            let fallback = if parts.len() > 1 {
                Some(parts[1..].join(","))
            } else {
                None
            };
            if let Some(target) = vars.get(name)
                && visiting.insert(name.to_owned())
            {
                let resolved = resolve_var_expr(target, vars, visiting);
                visiting.remove(name);
                out.push_str(&resolved);
            } else if let Some(fb) = fallback {
                out.push_str(&resolve_var_expr(&fb, vars, visiting));
            } else {
                out.push_str(&expr[i..end]);
            }
            i = end;
            continue;
        }
        let ch = expr[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn take_balanced_parens(s: &str, start: usize) -> Option<(usize, &str)> {
    let mut depth = 1usize;
    let mut i = start;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((i + 1, &s[start..i]));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Maps `--app-theme-<kebab-role>` or direct role variables (`--canvas`, `--surface`, etc.)
/// to the camelCase `THEME_COLOR_ROLES` name.
pub fn custom_prop_to_role(prop: &str) -> Option<&'static str> {
    let suffix = prop
        .strip_prefix("--app-theme-")
        .or_else(|| prop.strip_prefix("--"))?;
    match suffix {
        "canvas" => Some("canvas"),
        "chrome" => Some("chrome"),
        "toolbar" => Some("toolbar"),
        "toolbar-foreground" if prop.starts_with("--app-theme-") => Some("toolbarForeground"),
        "toolbar-border" if prop.starts_with("--app-theme-") => Some("toolbarBorder"),
        "toolbar-control" if prop.starts_with("--app-theme-") => Some("toolbarControl"),
        "toolbar-control-foreground" if prop.starts_with("--app-theme-") => Some("toolbarControlForeground"),
        "toolbar-control-hover" if prop.starts_with("--app-theme-") => Some("toolbarControlHover"),
        "surface" => Some("surface"),
        "surface-raised" if prop.starts_with("--app-theme-") => Some("surfaceRaised"),
        "surface-overlay" => Some("surfaceOverlay"),
        "text" => Some("text"),
        "text-muted" => Some("textMuted"),
        "border" if prop.starts_with("--app-theme-") => Some("border"),
        "input" if prop.starts_with("--app-theme-") => Some("input"),
        "focus" => Some("focus"),
        "accent" if prop.starts_with("--app-theme-") => Some("accent"),
        "accent-foreground" if prop.starts_with("--app-theme-") => Some("accentForeground"),
        "secondary" if prop.starts_with("--app-theme-") => Some("secondary"),
        "secondary-foreground" if prop.starts_with("--app-theme-") => Some("secondaryForeground"),
        "muted" if prop.starts_with("--app-theme-") => Some("muted"),
        "muted-foreground" if prop.starts_with("--app-theme-") => Some("mutedForeground"),
        "placeholder" if prop.starts_with("--app-theme-") => Some("placeholder"),
        "secondary-label" if prop.starts_with("--app-theme-") => Some("secondaryLabel"),
        "icon-muted" if prop.starts_with("--app-theme-") => Some("iconMuted"),
        "error" if prop.starts_with("--app-theme-") => Some("error"),
        "error-foreground" if prop.starts_with("--app-theme-") => Some("errorForeground"),
        "error-surface" if prop.starts_with("--app-theme-") => Some("errorSurface"),
        "warning" if prop.starts_with("--app-theme-") => Some("warning"),
        "warning-foreground" if prop.starts_with("--app-theme-") => Some("warningForeground"),
        "warning-surface" if prop.starts_with("--app-theme-") => Some("warningSurface"),
        "update" if prop.starts_with("--app-theme-") => Some("update"),
        "update-foreground" if prop.starts_with("--app-theme-") => Some("updateForeground"),
        "update-surface" => Some("updateSurface"),
        "accent-surface" => Some("accentSurface"),
        "accent-surface-foreground" => Some("accentSurfaceForeground"),
        "message-surface" if prop.starts_with("--app-theme-") => Some("messageSurface"),
        "message-foreground" if prop.starts_with("--app-theme-") => Some("messageForeground"),
        "message-action" if prop.starts_with("--app-theme-") => Some("messageAction"),
        "message-action-foreground" if prop.starts_with("--app-theme-") => Some("messageActionForeground"),
        "message-action-hover" if prop.starts_with("--app-theme-") => Some("messageActionHover"),
        "code-background" if prop.starts_with("--app-theme-") => Some("codeBackground"),
        "code-foreground" if prop.starts_with("--app-theme-") => Some("codeForeground"),
        "search-match-background" if prop.starts_with("--app-theme-") => Some("searchMatchBackground"),
        "search-match-foreground" if prop.starts_with("--app-theme-") => Some("searchMatchForeground"),
        "search-match-active-background" if prop.starts_with("--app-theme-") => Some("searchMatchActiveBackground"),
        "search-match-active-foreground" if prop.starts_with("--app-theme-") => Some("searchMatchActiveForeground"),
        "sidebar" if prop.starts_with("--app-theme-") => Some("sidebar"),
        "sidebar-foreground" if prop.starts_with("--app-theme-") => Some("sidebarForeground"),
        "sidebar-muted-foreground" if prop.starts_with("--app-theme-") => Some("sidebarMutedForeground"),
        "sidebar-control-surface" if prop.starts_with("--app-theme-") => Some("sidebarControlSurface"),
        "sidebar-row-hover" if prop.starts_with("--app-theme-") => Some("sidebarRowHover"),
        "sidebar-row-active" if prop.starts_with("--app-theme-") => Some("sidebarRowActive"),
        "sidebar-row-selected" if prop.starts_with("--app-theme-") => Some("sidebarRowSelected"),
        "sidebar-border" if prop.starts_with("--app-theme-") => Some("sidebarBorder"),
        "terminal-background" if prop.starts_with("--app-theme-") => Some("terminalBackground"),
        "terminal-foreground" if prop.starts_with("--app-theme-") => Some("terminalForeground"),
        "terminal-cursor" if prop.starts_with("--app-theme-") => Some("terminalCursor"),
        "terminal-selection" => Some("terminalSelection"),
        "terminal-scrollbar" => Some("terminalScrollbar"),
        "terminal-scrollbar-hover" => Some("terminalScrollbarHover"),
        _ => None,
    }
}

/// Maps root semantic CSS custom properties (`index.css`) to native `Palette` field names.
fn root_prop_to_tokens(prop: &str) -> &'static [&'static str] {
    match prop {
        "--app-chrome-background" => &["app_chrome_background"],
        "--background" | "--color-background" => &["background"],
        "--foreground" | "--color-foreground" => &["foreground"],
        "--surface-raised" => &["surface_raised"],
        "--card" | "--color-card" => &["card"],
        "--card-foreground" | "--color-card-foreground" => &["card_foreground"],
        "--popover" | "--color-popover" => &["popover"],
        "--popover-foreground" | "--color-popover-foreground" => &["popover_foreground"],
        "--primary" | "--color-primary" => &["primary"],
        "--primary-foreground" | "--color-primary-foreground" => &["primary_foreground"],
        "--secondary" | "--color-secondary" => &["secondary"],
        "--secondary-foreground" | "--color-secondary-foreground" => &["secondary_foreground"],
        "--muted" | "--color-muted" => &["muted"],
        "--muted-foreground" | "--color-muted-foreground" => &["muted_foreground"],
        "--accent" | "--color-accent" => &["accent"],
        "--accent-foreground" | "--color-accent-foreground" => &["accent_foreground"],
        "--placeholder" | "--color-placeholder" => &["placeholder"],
        "--secondary-label" | "--color-secondary-label" => &["secondary_label"],
        "--icon-muted" | "--color-icon-muted" => &["icon_muted"],
        "--border" | "--color-border" => &["border"],
        "--input" | "--color-input" => &["input"],
        "--ring" | "--color-ring" => &["ring"],
        "--destructive" | "--color-destructive" => &["destructive"],
        "--destructive-foreground" | "--color-destructive-foreground" => &["destructive_foreground"],
        "--error" | "--color-error" => &["error", "destructive"],
        "--error-foreground" | "--color-error-foreground" => &["error_foreground", "destructive_foreground"],
        "--error-surface" => &["error_surface"],
        "--tool-error-icon" => &["tool_error_icon"],
        "--warning" | "--color-warning" => &["warning"],
        "--warning-foreground" | "--color-warning-foreground" => &["warning_foreground"],
        "--warning-surface" => &["warning_surface"],
        "--success" | "--color-success" => &["success"],
        "--success-foreground" | "--color-success-foreground" => &["success_foreground"],
        "--info" | "--color-info" => &["info"],
        "--info-foreground" | "--color-info-foreground" => &["info_foreground"],
        "--update" => &["update"],
        "--update-foreground" => &["update_foreground"],
        "--guest" => &["guest"],
        "--guest-foreground" => &["guest_foreground"],
        "--guest-surface" => &["guest_surface"],
        "--message-surface" => &["message_surface"],
        "--message-foreground" | "--color-message-foreground" => &["message_foreground"],
        "--message-action" => &["message_action"],
        "--message-action-foreground" => &["message_action_foreground"],
        "--message-action-hover" => &["message_action_hover"],
        "--code-background" => &["code_background", "code_block_background"],
        "--code-foreground" => &["code_foreground"],
        "--diff-addition" => &["diff_addition"],
        "--diff-deletion" => &["diff_deletion"],
        "--diff-addition-foreground" => &["diff_addition_foreground"],
        "--diff-deletion-foreground" => &["diff_deletion_foreground"],
        "--diff-modified-icon" => &["diff_modified_icon"],
        "--toolbar-background" => &["toolbar_background"],
        "--toolbar-foreground" | "--contrast-toolbar-foreground" => &["toolbar_foreground"],
        "--toolbar-border" | "--contrast-toolbar-border" => &["toolbar_border"],
        "--toolbar-control" => &["toolbar_control"],
        "--toolbar-control-foreground" | "--contrast-toolbar-control-foreground" => &["toolbar_control_foreground"],
        "--toolbar-control-hover" => &["toolbar_control_hover"],
        "--terminal-background" => &["terminal_background"],
        "--terminal-foreground" => &["terminal_foreground"],
        "--terminal-cursor" => &["terminal_cursor"],
        "--terminal-selection-background" => &["terminal_selection"],
        "--app-scrollbar-thumb" => &["scrollbar_thumb"],
        "--app-scrollbar-thumb-hover" => &["scrollbar_thumb_hover"],
        "--sidebar" => &["sidebar"],
        "--sidebar-foreground" | "--color-sidebar-foreground" => &["sidebar_foreground"],
        "--sidebar-muted-foreground" | "--color-sidebar-muted-foreground" => &["sidebar_muted_foreground"],
        "--sidebar-icon-color" => &["sidebar_icon"],
        "--sidebar-control-surface" => &["sidebar_control_surface"],
        "--sidebar-row-hover" => &["sidebar_row_hover"],
        "--sidebar-row-active" => &["sidebar_row_active"],
        "--sidebar-row-selected" => &["sidebar_row_selected"],
        "--sidebar-border" | "--color-sidebar-border" => &["sidebar_border", "sidebar_edge"],
        _ => &[],
    }
}

fn sidebar_prop_to_tokens(prop: &str) -> &'static [&'static str] {
    match prop {
        "--background" | "--sidebar" => &["sidebar"],
        "--foreground" | "--color-foreground" | "--sidebar-foreground" | "--color-sidebar-foreground" => {
            &["sidebar_foreground"]
        }
        "--muted-foreground" | "--color-muted-foreground" | "--sidebar-muted-foreground" => {
            &["sidebar_muted_foreground"]
        }
        "--sidebar-icon-color" => &["sidebar_icon"],
        "--secondary-label" | "--color-secondary-label" => &["sidebar_secondary_label"],
        "--accent" | "--color-accent" => &["sidebar_accent"],
        "--sidebar-control-surface" => &["sidebar_control_surface"],
        "--sidebar-row-hover" => &["sidebar_row_hover"],
        "--sidebar-row-active" => &["sidebar_row_active"],
        "--sidebar-row-selected" => &["sidebar_row_selected"],
        "--border" | "--color-border" | "--sidebar-border" | "--color-sidebar-border" => {
            &["sidebar_border", "sidebar_edge"]
        }
        _ => &[],
    }
}

/// 1-based `(line, column)` for a UTF-8 byte offset (`ThemeCssEditor.tsx` `updatePosition`).
pub fn line_and_column(text: &str, byte_offset: usize) -> (usize, usize) {
    let clamped = clamp_char_boundary(text, byte_offset);
    let prefix = &text[..clamped];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let col_slice = match prefix.rfind('\n') {
        Some(idx) => &prefix[idx + 1..],
        None => prefix,
    };
    (line, col_slice.chars().count() + 1)
}

fn clamp_char_boundary(text: &str, offset: usize) -> usize {
    let mut pos = offset.min(text.len());
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditResult {
    pub text: String,
    pub selection: Range<usize>,
}

fn leading_unindent_len(line: &str) -> usize {
    if line.starts_with("  ") {
        2
    } else if line.starts_with(' ') || line.starts_with('\t') {
        1
    } else {
        0
    }
}

/// `ThemeCssEditor.tsx` `Tab` / `Shift+Tab` indentation behavior.
pub fn indent_selection(text: &str, selection: Range<usize>, unindent: bool) -> Option<EditResult> {
    let start = clamp_char_boundary(text, selection.start.min(selection.end));
    let end = clamp_char_boundary(text, selection.start.max(selection.end));
    let line_start = text[..start].rfind('\n').map_or(0, |idx| idx + 1);

    if start == end && !unindent {
        let mut next = String::with_capacity(text.len() + 2);
        next.push_str(&text[..start]);
        next.push_str("  ");
        next.push_str(&text[end..]);
        return Some(EditResult {
            text: next,
            selection: (start + 2)..(start + 2),
        });
    }

    if start == end && unindent {
        let remove = leading_unindent_len(&text[line_start..]);
        if remove == 0 {
            return None;
        }
        let mut next = String::with_capacity(text.len() - remove);
        next.push_str(&text[..line_start]);
        next.push_str(&text[line_start + remove..]);
        let cursor = start.saturating_sub(remove).max(line_start);
        return Some(EditResult {
            text: next,
            selection: cursor..cursor,
        });
    }

    // A selection ending right after '\n' leaves that next line untouched.
    let block_end = if end > start && text.as_bytes()[end - 1] == b'\n' {
        end - 1
    } else {
        end
    };
    let selected = &text[line_start..block_end];
    let replacement = selected
        .split('\n')
        .map(|line| {
            if unindent {
                let remove = leading_unindent_len(line);
                line[remove..].to_owned()
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut next = String::with_capacity(text.len() + replacement.len());
    next.push_str(&text[..line_start]);
    next.push_str(&replacement);
    next.push_str(&text[block_end..]);
    let new_end = line_start + replacement.len();
    Some(EditResult {
        text: next,
        selection: line_start..new_end,
    })
}

/// `ThemeCssEditor.tsx` `Enter` auto-indent behavior (preserves indent, adds 2 spaces after `{`).
pub fn newline_with_indent(text: &str, cursor: usize) -> EditResult {
    let pos = clamp_char_boundary(text, cursor);
    let line_start = text[..pos].rfind('\n').map_or(0, |idx| idx + 1);
    let preceding_line = &text[line_start..pos];
    let indent: String = preceding_line
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let extra = if preceding_line.trim_end().ends_with('{') {
        "  "
    } else {
        ""
    };
    let insertion = format!("\n{indent}{extra}");
    let mut next = String::with_capacity(text.len() + insertion.len());
    next.push_str(&text[..pos]);
    next.push_str(&insertion);
    next.push_str(&text[pos..]);
    let next_cursor = pos + insertion.len();
    EditResult {
        text: next,
        selection: next_cursor..next_cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::themes;

    #[test]
    fn value_fetches_and_remote_file_checks_match_web_shared() {
        assert!(!value_fetches("oklch(0.5 0.2 260)"));
        assert!(!value_fetches("url(data:image/png;base64,AAAA)"));
        assert!(!value_fetches("url(  'data:image/svg+xml,<svg/>')"));
        assert!(!value_fetches("url(#grad)"));
        assert!(value_fetches("url(https://evil.example/leak)"));
        assert!(value_fetches("url(\u{00a0}data:image/png;base64,AA)"));
        assert!(value_fetches("image-set(url(data:image/png;base64,AA) 1x)"));
        assert!(value_fetches("src(\"https://evil.example\")"));
        assert!(value_fetches(r"\75rl(https://evil.example)"));

        assert!(!loads_remote_files("/* @import url(https://x) */ :root { --background: #111; }"));
        assert!(loads_remote_files("@import 'https://evil.example/a.css';"));
        assert!(loads_remote_files(":root { background: url(https://evil.example/bg.png); }"));
        assert!(!loads_remote_files(":root { background: url(data:image/png;base64,AA); }"));
    }

    #[test]
    fn parses_mode_scoped_custom_properties_and_ignores_fetches_and_dom_rules() {
        let css = r#"
            @import url("https://evil.example/a.css");
            :root {
                --background: #fafafa;
                --brand: #112233;
                --primary: var(--brand);
                --canvas: #f0f0f0;
                --blocked: url(https://evil.example/leak);
            }
            .dark {
                --background: #0a0b0c;
                --app-theme-canvas: #101112;
            }
            :root:not(.dark) {
                --border: #dddddd !important;
                --border: #999999;
            }
            [data-app-sidebar] {
                --background: #123456;
            }
            [data-slot="button"] {
                border-radius: 999px;
            }
        "#;
        let parsed = ParsedCss::parse(css);
        let diag = parsed.diagnostics();
        assert!(diag.loads_remote_files);
        assert!(!diag.exceeds_max_length);
        assert!(diag.applied_variables >= 5);
        assert_eq!(diag.unmapped_dom_rules, 1);

        let light_roles = parsed.role_overrides(false, &themes::standard(false));
        assert_eq!(light_roles.get("canvas").map(String::as_str), Some("#f0f0f0ff"));

        let dark_roles = parsed.role_overrides(true, &themes::standard(true));
        assert_eq!(dark_roles.get("canvas").map(String::as_str), Some("#101112ff"));

        let light_tokens = parsed.token_overrides(false, &themes::standard(false));
        assert_eq!(light_tokens.get("background"), Some(&0xfafafaff));
        assert_eq!(light_tokens.get("primary"), Some(&0x112233ff));
        assert_eq!(light_tokens.get("border"), Some(&0xddddddff));
        assert_eq!(light_tokens.get("sidebar"), Some(&0x123456ff));

        let dark_tokens = parsed.token_overrides(true, &themes::standard(true));
        assert_eq!(dark_tokens.get("background"), Some(&0x0a0b0cff));
        assert_eq!(dark_tokens.get("border"), None);
    }

    #[test]
    fn css_editor_indentation_and_cursor_position_match_web() {
        assert_eq!(line_and_column(":root {\n  --a: 1;\n}", 0), (1, 1));
        assert_eq!(line_and_column(":root {\n  --a: 1;\n}", 10), (2, 3));

        let indented = indent_selection(":root {", 0..0, false).unwrap();
        assert_eq!(indented.text, "  :root {");
        assert_eq!(indented.selection, 2..2);

        let unindented = indent_selection("  :root {", 4..4, true).unwrap();
        assert_eq!(unindented.text, ":root {");
        assert_eq!(unindented.selection, 2..2);

        // Multi-line selection ending right after '\n' leaves the trailing line untouched.
        let block = indent_selection("a\nb\nc", 0..4, false).unwrap();
        assert_eq!(block.text, "  a\n  b\nc");
        assert_eq!(block.selection, 0..7);

        let enter = newline_with_indent("  :root {", 9);
        assert_eq!(enter.text, "  :root {\n    ");
        assert_eq!(enter.selection, 14..14);
    }
}
