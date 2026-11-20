//! Typed `THEME_CSS_HOOKS` slot selectors, CSS property parsing, and compiled `SlotStylesheet`.
//!
//! Mirrors the 29 stable selectors in `packages/shared/src/themeCss.ts` (`THEME_CSS_HOOKS`) so
//! theme CSS rules targeting named UI surfaces compile once per theme/mode change into typed,
//! safety-clamped style overrides for GPUI.

use std::collections::{BTreeMap, BTreeSet};

use super::{Colors, color, css};

/// Safety limits enforced on compiled CSS slot overrides.
pub const MAX_BLUR_PX: f32 = 256.0;
pub const MAX_BORDER_WIDTH_PX: f32 = 64.0;
pub const MIN_FONT_SIZE_PX: f32 = 8.0;
pub const MAX_FONT_SIZE_PX: f32 = 96.0;
pub const PILL_RADIUS_PX: f32 = 9999.0;

const ROOT_REM_PX: f32 = 16.0;

/// All 29 stable UI hooks from `packages/shared/src/themeCss.ts` (`THEME_CSS_HOOKS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ThemeSlot {
    Sidebar = 0,
    SidebarInner = 1,
    ThreadItem = 2,
    ProjectMonogram = 3,
    ChatHeader = 4,
    ConversationArea = 5,
    MessagesTimelineScroll = 6,
    TimelineRow = 7,
    UserMessageRow = 8,
    UserMessageBubble = 9,
    AssistantMessageRow = 10,
    ChatMarkdown = 11,
    ChatMarkdownCodeblock = 12,
    ChatComposerForm = 13,
    ComposerHost = 14,
    ComposerTiptap = 15,
    ThreadDetailsCard = 16,
    ThreadTerminalDrawer = 17,
    SettingsPageScroll = 18,
    Button = 19,
    Input = 20,
    Switch = 21,
    SelectTrigger = 22,
    DialogPopup = 23,
    PopoverPopup = 24,
    MenuPopup = 25,
    TooltipPopup = 26,
    ScrollAreaScrollbar = 27,
    ScrollAreaThumb = 28,
}

impl ThemeSlot {
    pub const COUNT: usize = 29;

    pub const ALL: [Self; Self::COUNT] = [
        Self::Sidebar,
        Self::SidebarInner,
        Self::ThreadItem,
        Self::ProjectMonogram,
        Self::ChatHeader,
        Self::ConversationArea,
        Self::MessagesTimelineScroll,
        Self::TimelineRow,
        Self::UserMessageRow,
        Self::UserMessageBubble,
        Self::AssistantMessageRow,
        Self::ChatMarkdown,
        Self::ChatMarkdownCodeblock,
        Self::ChatComposerForm,
        Self::ComposerHost,
        Self::ComposerTiptap,
        Self::ThreadDetailsCard,
        Self::ThreadTerminalDrawer,
        Self::SettingsPageScroll,
        Self::Button,
        Self::Input,
        Self::Switch,
        Self::SelectTrigger,
        Self::DialogPopup,
        Self::PopoverPopup,
        Self::MenuPopup,
        Self::TooltipPopup,
        Self::ScrollAreaScrollbar,
        Self::ScrollAreaThumb,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    /// Canonical selector string matching `THEME_CSS_HOOKS` in `packages/shared/src/themeCss.ts`.
    pub const fn selector(self) -> &'static str {
        match self {
            Self::Sidebar => r#"[data-slot="sidebar"]"#,
            Self::SidebarInner => r#"[data-slot="sidebar-inner"]"#,
            Self::ThreadItem => "[data-thread-item]",
            Self::ProjectMonogram => r#"[data-slot="project-monogram"]"#,
            Self::ChatHeader => "[data-chat-header]",
            Self::ConversationArea => r#"[data-workspace-area="conversation"]"#,
            Self::MessagesTimelineScroll => ".messages-timeline-scroll",
            Self::TimelineRow => "[data-timeline-row-kind]",
            Self::UserMessageRow => r#"[data-message-role="user"]"#,
            Self::UserMessageBubble => "[data-user-message-bubble]",
            Self::AssistantMessageRow => r#"[data-message-role="assistant"]"#,
            Self::ChatMarkdown => ".chat-markdown",
            Self::ChatMarkdownCodeblock => ".chat-markdown-codeblock",
            Self::ChatComposerForm => "[data-chat-composer-form]",
            Self::ComposerHost => r#"[data-slot="composer-host"]"#,
            Self::ComposerTiptap => ".composer-tiptap",
            Self::ThreadDetailsCard => "[data-thread-details-card]",
            Self::ThreadTerminalDrawer => "[data-thread-terminal-drawer]",
            Self::SettingsPageScroll => "[data-settings-page-scroll]",
            Self::Button => r#"[data-slot="button"]"#,
            Self::Input => r#"[data-slot="input"]"#,
            Self::Switch => r#"[data-slot="switch"]"#,
            Self::SelectTrigger => r#"[data-slot="select-trigger"]"#,
            Self::DialogPopup => r#"[data-slot="dialog-popup"]"#,
            Self::PopoverPopup => r#"[data-slot="popover-popup"]"#,
            Self::MenuPopup => r#"[data-slot="menu-popup"]"#,
            Self::TooltipPopup => r#"[data-slot="tooltip-popup"]"#,
            Self::ScrollAreaScrollbar => r#"[data-slot="scroll-area-scrollbar"]"#,
            Self::ScrollAreaThumb => r#"[data-slot="scroll-area-thumb"]"#,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotState {
    Base,
    Hover,
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotSelectorMatch {
    pub slot: ThemeSlot,
    pub row_kind: Option<String>,
    pub state: SlotState,
    pub appearance: css::AppearanceScope,
    pub specificity: (u16, u16, u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotDecl {
    pub slot: ThemeSlot,
    pub row_kind: Option<String>,
    pub state: SlotState,
    pub appearance: css::AppearanceScope,
    pub specificity: (u16, u16, u16),
    pub property: String,
    pub value: String,
    pub important: bool,
    pub order: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotVarDecl {
    pub slot: ThemeSlot,
    pub row_kind: Option<String>,
    pub appearance: css::AppearanceScope,
    pub specificity: (u16, u16, u16),
    pub name: String,
    pub value: String,
    pub important: bool,
    pub order: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SlotLength {
    Px(f32),
    Rem(f32),
    Fraction(f32),
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SlotSizeConstraint {
    Px(f32),
    Rem(f32),
    Fraction(f32),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotEdges<T> {
    pub top: Option<T>,
    pub right: Option<T>,
    pub bottom: Option<T>,
    pub left: Option<T>,
}

impl<T> Default for SlotEdges<T> {
    fn default() -> Self {
        Self { top: None, right: None, bottom: None, left: None }
    }
}

impl<T> SlotEdges<T> {
    pub fn is_empty(&self) -> bool {
        self.top.is_none() && self.right.is_none() && self.bottom.is_none() && self.left.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotCorners<T> {
    pub top_left: Option<T>,
    pub top_right: Option<T>,
    pub bottom_right: Option<T>,
    pub bottom_left: Option<T>,
}

impl<T> Default for SlotCorners<T> {
    fn default() -> Self {
        Self { top_left: None, top_right: None, bottom_right: None, bottom_left: None }
    }
}

impl<T> SlotCorners<T> {
    pub fn is_empty(&self) -> bool {
        self.top_left.is_none()
            && self.top_right.is_none()
            && self.bottom_right.is_none()
            && self.bottom_left.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundImageSize {
    Cover,
    Contain,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    pub color: u32,
    pub position: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SlotBackground {
    Solid(u32),
    LinearGradient {
        angle_deg: f32,
        stops: Vec<GradientStop>,
    },
    ThemeBackgroundImage {
        size: BackgroundImageSize,
        overlay_color: Option<u32>,
    },
    Transparent,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotBoxShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub spread_radius: f32,
    pub color: u32,
    pub inset: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SlotLineHeight {
    Px(f32),
    Rem(f32),
    Relative(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotFlexDirection {
    Row,
    Column,
    RowReverse,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotAlignItems {
    Start,
    End,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotJustifyContent {
    Start,
    End,
    FlexStart,
    FlexEnd,
    Center,
    Stretch,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlotStyleOverride {
    pub display_none: Option<bool>,
    pub background: Option<SlotBackground>,
    pub color: Option<u32>,
    pub border_color: Option<u32>,
    pub border_widths: SlotEdges<f32>,
    pub border_radii: SlotCorners<SlotLength>,
    pub padding: SlotEdges<SlotLength>,
    pub margin: SlotEdges<SlotLength>,
    pub gap: Option<SlotLength>,
    pub row_gap: Option<SlotLength>,
    pub column_gap: Option<SlotLength>,
    pub width: Option<SlotLength>,
    pub min_width: Option<SlotLength>,
    pub max_width: Option<SlotSizeConstraint>,
    pub height: Option<SlotLength>,
    pub min_height: Option<SlotLength>,
    pub max_height: Option<SlotSizeConstraint>,
    pub box_shadows: Option<Vec<SlotBoxShadow>>,
    pub opacity: Option<f32>,
    pub font_family: Option<String>,
    pub font_size: Option<f32>,
    pub font_weight: Option<u16>,
    pub line_height: Option<SlotLineHeight>,
    pub flex_direction: Option<SlotFlexDirection>,
    pub align_items: Option<SlotAlignItems>,
    pub justify_content: Option<SlotJustifyContent>,
}

impl SlotStyleOverride {
    pub fn is_empty(&self) -> bool {
        self.display_none.is_none()
            && self.background.is_none()
            && self.color.is_none()
            && self.border_color.is_none()
            && self.border_widths.is_empty()
            && self.border_radii.is_empty()
            && self.padding.is_empty()
            && self.margin.is_empty()
            && self.gap.is_none()
            && self.row_gap.is_none()
            && self.column_gap.is_none()
            && self.width.is_none()
            && self.min_width.is_none()
            && self.max_width.is_none()
            && self.height.is_none()
            && self.min_height.is_none()
            && self.max_height.is_none()
            && self.box_shadows.is_none()
            && self.opacity.is_none()
            && self.font_family.is_none()
            && self.font_size.is_none()
            && self.font_weight.is_none()
            && self.line_height.is_none()
            && self.flex_direction.is_none()
            && self.align_items.is_none()
            && self.justify_content.is_none()
    }

    pub fn has_scoped_background_image(&self) -> bool {
        matches!(self.background, Some(SlotBackground::ThemeBackgroundImage { .. }))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlotStyleSet {
    pub base: SlotStyleOverride,
    pub hover: SlotStyleOverride,
    pub active: SlotStyleOverride,
}

impl SlotStyleSet {
    pub fn is_empty(&self) -> bool {
        self.base.is_empty() && self.hover.is_empty() && self.active.is_empty()
    }

    pub fn has_scoped_background_image(&self) -> bool {
        self.base.has_scoped_background_image()
            || self.hover.has_scoped_background_image()
            || self.active.has_scoped_background_image()
    }
}

/// Pre-compiled, appearance-resolved slot styles indexed by [`ThemeSlot`].
#[derive(Debug, Clone, PartialEq)]
pub struct SlotStylesheet {
    slots: Box<[Option<SlotStyleSet>; ThemeSlot::COUNT]>,
    timeline_rows: BTreeMap<String, SlotStyleSet>,
    empty: bool,
    has_scoped_bg_image: bool,
}

impl Default for SlotStylesheet {
    fn default() -> Self {
        Self::empty()
    }
}

impl SlotStylesheet {
    pub fn empty() -> Self {
        Self {
            slots: Box::new(std::array::from_fn(|_| None)),
            timeline_rows: BTreeMap::new(),
            empty: true,
            has_scoped_bg_image: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.empty
    }

    pub fn get(&self, slot: ThemeSlot) -> Option<&SlotStyleSet> {
        self.slots[slot.index()].as_ref()
    }

    /// Resolves styles for a specific `data-timeline-row-kind` value, falling back to generic
    /// `[data-timeline-row-kind]` overrides when no kind-specific rules exist.
    pub fn timeline_row(&self, kind: &str) -> Option<&SlotStyleSet> {
        self.timeline_rows
            .get(&kind.to_ascii_lowercase())
            .or_else(|| self.get(ThemeSlot::TimelineRow))
    }

    /// True when any compiled slot references `var(--theme-background-image)`.
    pub fn has_scoped_background_image(&self) -> bool {
        self.has_scoped_bg_image
    }
}

/// Whether `prop` (case-insensitive CSS property name) is compiled into [`SlotStyleOverride`].
pub(crate) fn is_supported_slot_property(prop: &str) -> bool {
    matches!(
        prop.to_ascii_lowercase().as_str(),
        "display"
            | "background"
            | "background-color"
            | "background-image"
            | "background-size"
            | "color"
            | "border"
            | "border-top"
            | "border-right"
            | "border-bottom"
            | "border-left"
            | "border-width"
            | "border-top-width"
            | "border-right-width"
            | "border-bottom-width"
            | "border-left-width"
            | "border-color"
            | "border-top-color"
            | "border-right-color"
            | "border-bottom-color"
            | "border-left-color"
            | "border-radius"
            | "border-top-left-radius"
            | "border-top-right-radius"
            | "border-bottom-right-radius"
            | "border-bottom-left-radius"
            | "padding"
            | "padding-top"
            | "padding-right"
            | "padding-bottom"
            | "padding-left"
            | "padding-inline"
            | "padding-block"
            | "padding-inline-start"
            | "padding-inline-end"
            | "padding-block-start"
            | "padding-block-end"
            | "margin"
            | "margin-top"
            | "margin-right"
            | "margin-bottom"
            | "margin-left"
            | "margin-inline"
            | "margin-block"
            | "margin-inline-start"
            | "margin-inline-end"
            | "margin-block-start"
            | "margin-block-end"
            | "gap"
            | "row-gap"
            | "column-gap"
            | "width"
            | "min-width"
            | "max-width"
            | "height"
            | "min-height"
            | "max-height"
            | "box-shadow"
            | "opacity"
            | "font-family"
            | "font-size"
            | "font-weight"
            | "line-height"
            | "flex-direction"
            | "align-items"
            | "justify-content"
    )
}

// ---------------------------------------------------------------------------
// Selector matching & specificity
// ---------------------------------------------------------------------------

pub(crate) fn classify_slot_selectors(selectors: &[String]) -> Vec<SlotSelectorMatch> {
    if selectors.is_empty() {
        return Vec::new();
    }
    let mut expanded: Vec<String> = vec![String::new()];
    for layer in selectors {
        let parts = css::split_selector_list(layer);
        let mut next = Vec::new();
        for base in &expanded {
            for part in &parts {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let combined = if base.is_empty() {
                    part.to_owned()
                } else if part.contains('&') {
                    part.replace('&', base)
                } else {
                    format!("{base} {part}")
                };
                next.push(combined);
            }
        }
        expanded = next;
    }

    let mut out = Vec::new();
    for sel in expanded {
        if let Some(matched) = match_single_slot_selector(&sel) {
            out.push(matched);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Combinator {
    Descendant,
    Child,
    Unsupported,
}

fn split_complex_selector(sel: &str) -> Vec<(Option<Combinator>, &str)> {
    let bytes = sel.as_bytes();
    let mut parts = Vec::new();
    let mut i = 0;
    let mut pending_comb: Option<Combinator> = None;

    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if matches!(bytes[i], b'>' | b'+' | b'~') {
            pending_comb = Some(if bytes[i] == b'>' {
                Combinator::Child
            } else {
                Combinator::Unsupported
            });
            i += 1;
            continue;
        }
        let start = i;
        let mut paren = 0usize;
        let mut bracket = 0usize;
        let mut quote: Option<u8> = None;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = quote {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
                i += 1;
                continue;
            }
            match b {
                b'"' | b'\'' => quote = Some(b),
                b'(' => paren += 1,
                b')' => paren = paren.saturating_sub(1),
                b'[' => bracket += 1,
                b']' => bracket = bracket.saturating_sub(1),
                b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'+' | b'~' if paren == 0 && bracket == 0 => {
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        let segment = &sel[start..i];
        if !segment.is_empty() {
            let comb = if parts.is_empty() {
                None
            } else {
                Some(pending_comb.unwrap_or(Combinator::Descendant))
            };
            parts.push((comb, segment));
            pending_comb = None;
        }
    }
    parts
}

#[derive(Debug)]
enum SimpleToken<'a> {
    Type(&'a str),
    Universal,
    Id,
    Class(&'a str),
    Attr(&'a str, Option<&'a str>),
    PseudoClass(&'a str, Option<&'a str>),
    PseudoElement(&'a str),
}

fn tokenize_compound(compound: &str) -> Option<Vec<SimpleToken<'_>>> {
    let bytes = compound.as_bytes();
    let mut i = 0;
    let mut tokens = Vec::new();

    while i < bytes.len() {
        match bytes[i] {
            b'*' => {
                tokens.push(SimpleToken::Universal);
                i += 1;
            }
            b'#' => {
                let start = i + 1;
                i = scan_ident(bytes, start);
                if i == start {
                    return None;
                }
                tokens.push(SimpleToken::Id);
            }
            b'.' => {
                let start = i + 1;
                i = scan_ident(bytes, start);
                if i == start {
                    return None;
                }
                tokens.push(SimpleToken::Class(&compound[start..i]));
            }
            b'[' => {
                let start = i + 1;
                let mut j = start;
                let mut quote: Option<u8> = None;
                while j < bytes.len() {
                    let b = bytes[j];
                    if let Some(q) = quote {
                        if b == b'\\' && j + 1 < bytes.len() {
                            j += 2;
                            continue;
                        }
                        if b == q {
                            quote = None;
                        }
                    } else if b == b'"' || b == b'\'' {
                        quote = Some(b);
                    } else if b == b']' {
                        break;
                    }
                    j += 1;
                }
                if j >= bytes.len() || bytes[j] != b']' {
                    return None;
                }
                let inner = compound[start..j].trim();
                let (name, val) = if let Some((k, v)) = inner.split_once('=') {
                    let k = k.trim();
                    if k.ends_with(['~', '|', '^', '$', '*']) {
                        return None;
                    }
                    let v = v.trim();
                    let unquoted = strip_quotes(v);
                    (k, Some(unquoted))
                } else {
                    (inner, None)
                };
                if name.is_empty() {
                    return None;
                }
                tokens.push(SimpleToken::Attr(name, val));
                i = j + 1;
            }
            b':' => {
                let double = i + 1 < bytes.len() && bytes[i + 1] == b':';
                let start = i + if double { 2 } else { 1 };
                let end = scan_ident(bytes, start);
                if end == start {
                    return None;
                }
                let name = &compound[start..end];
                if double {
                    tokens.push(SimpleToken::PseudoElement(name));
                    i = end;
                } else if end < bytes.len() && bytes[end] == b'(' {
                    let (close, inner) = css::take_balanced_parens(compound, end + 1)?;
                    tokens.push(SimpleToken::PseudoClass(name, Some(inner.trim())));
                    i = close;
                } else {
                    tokens.push(SimpleToken::PseudoClass(name, None));
                    i = end;
                }
            }
            b if is_css_ident_start(b) => {
                let start = i;
                i = scan_ident(bytes, start);
                tokens.push(SimpleToken::Type(&compound[start..i]));
            }
            _ => return None,
        }
    }

    Some(tokens)
}

fn is_css_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'-'
}

fn scan_ident(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
    {
        i += 1;
    }
    i
}

fn strip_quotes(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn combine_appearance(a: css::AppearanceScope, b: css::AppearanceScope) -> Option<css::AppearanceScope> {
    match (a, b) {
        (css::AppearanceScope::Any, x) | (x, css::AppearanceScope::Any) => Some(x),
        (x, y) if x == y => Some(x),
        _ => None,
    }
}

fn add_spec(a: (u16, u16, u16), b: (u16, u16, u16)) -> (u16, u16, u16) {
    (a.0.saturating_add(b.0), a.1.saturating_add(b.1), a.2.saturating_add(b.2))
}

fn match_slot_token(token: &SimpleToken<'_>) -> Option<(ThemeSlot, Option<String>)> {
    match token {
        SimpleToken::Attr(name, val) => {
            let name_lower = name.to_ascii_lowercase();
            let val_lower = val.map(str::to_ascii_lowercase);
            match (name_lower.as_str(), val_lower.as_deref()) {
                ("data-slot", Some("sidebar")) | ("data-app-sidebar", None) => {
                    Some((ThemeSlot::Sidebar, None))
                }
                ("data-slot", Some("sidebar-inner")) => Some((ThemeSlot::SidebarInner, None)),
                ("data-thread-item", None) => Some((ThemeSlot::ThreadItem, None)),
                ("data-slot", Some("project-monogram")) => Some((ThemeSlot::ProjectMonogram, None)),
                ("data-chat-header", None) => Some((ThemeSlot::ChatHeader, None)),
                ("data-workspace-area", Some("conversation")) => {
                    Some((ThemeSlot::ConversationArea, None))
                }
                ("data-timeline-row-kind", None) => Some((ThemeSlot::TimelineRow, None)),
                ("data-timeline-row-kind", Some(kind)) if !kind.is_empty() => {
                    Some((ThemeSlot::TimelineRow, Some(kind.to_owned())))
                }
                ("data-message-role", Some("user")) => Some((ThemeSlot::UserMessageRow, None)),
                ("data-user-message-bubble", None) => Some((ThemeSlot::UserMessageBubble, None)),
                ("data-message-role", Some("assistant")) => {
                    Some((ThemeSlot::AssistantMessageRow, None))
                }
                ("data-chat-composer-form", None) => Some((ThemeSlot::ChatComposerForm, None)),
                ("data-slot", Some("composer-host")) => Some((ThemeSlot::ComposerHost, None)),
                ("data-thread-details-card", None) => Some((ThemeSlot::ThreadDetailsCard, None)),
                ("data-thread-terminal-drawer", None) => {
                    Some((ThemeSlot::ThreadTerminalDrawer, None))
                }
                ("data-settings-page-scroll", None) => Some((ThemeSlot::SettingsPageScroll, None)),
                ("data-slot", Some("button")) => Some((ThemeSlot::Button, None)),
                ("data-slot", Some("input")) => Some((ThemeSlot::Input, None)),
                ("data-slot", Some("switch")) => Some((ThemeSlot::Switch, None)),
                ("data-slot", Some("select-trigger")) => Some((ThemeSlot::SelectTrigger, None)),
                ("data-slot", Some("dialog-popup")) => Some((ThemeSlot::DialogPopup, None)),
                ("data-slot", Some("popover-popup")) => Some((ThemeSlot::PopoverPopup, None)),
                ("data-slot", Some("menu-popup")) => Some((ThemeSlot::MenuPopup, None)),
                ("data-slot", Some("tooltip-popup")) => Some((ThemeSlot::TooltipPopup, None)),
                ("data-slot", Some("scroll-area-scrollbar")) => {
                    Some((ThemeSlot::ScrollAreaScrollbar, None))
                }
                ("data-slot", Some("scroll-area-thumb")) => Some((ThemeSlot::ScrollAreaThumb, None)),
                _ => None,
            }
        }
        SimpleToken::Class(cls) => match cls.to_ascii_lowercase().as_str() {
            "messages-timeline-scroll" => Some((ThemeSlot::MessagesTimelineScroll, None)),
            "chat-markdown" => Some((ThemeSlot::ChatMarkdown, None)),
            "chat-markdown-codeblock" => Some((ThemeSlot::ChatMarkdownCodeblock, None)),
            "composer-tiptap" => Some((ThemeSlot::ComposerTiptap, None)),
            _ => None,
        },
        SimpleToken::PseudoElement(pe) => match pe.to_ascii_lowercase().as_str() {
            "-webkit-scrollbar" => Some((ThemeSlot::ScrollAreaScrollbar, None)),
            "-webkit-scrollbar-thumb" => Some((ThemeSlot::ScrollAreaThumb, None)),
            _ => None,
        },
        SimpleToken::PseudoClass(pc, None) => match pc.to_ascii_lowercase().as_str() {
            "-webkit-scrollbar" => Some((ThemeSlot::ScrollAreaScrollbar, None)),
            "-webkit-scrollbar-thumb" => Some((ThemeSlot::ScrollAreaThumb, None)),
            _ => None,
        },
        _ => None,
    }
}

fn match_appearance_token(token: &SimpleToken<'_>) -> Option<(css::AppearanceScope, (u16, u16, u16))> {
    match token {
        SimpleToken::Class(cls) if cls.eq_ignore_ascii_case("dark") => {
            Some((css::AppearanceScope::Dark, (0, 1, 0)))
        }
        SimpleToken::Class(cls) if cls.eq_ignore_ascii_case("light") => {
            Some((css::AppearanceScope::Light, (0, 1, 0)))
        }
        SimpleToken::Attr(name, Some(val)) if name.eq_ignore_ascii_case("data-theme") => {
            if val.eq_ignore_ascii_case("dark") {
                Some((css::AppearanceScope::Dark, (0, 1, 0)))
            } else if val.eq_ignore_ascii_case("light") {
                Some((css::AppearanceScope::Light, (0, 1, 0)))
            } else {
                None
            }
        }
        SimpleToken::PseudoClass(pc, Some(inner)) if pc.eq_ignore_ascii_case("not") => {
            let trimmed = inner.trim();
            if trimmed.eq_ignore_ascii_case(".dark") {
                Some((css::AppearanceScope::Light, (0, 1, 0)))
            } else if trimmed.eq_ignore_ascii_case(".light") {
                Some((css::AppearanceScope::Dark, (0, 1, 0)))
            } else if trimmed.eq_ignore_ascii_case(r#"[data-theme-id=""]"#)
                || trimmed.eq_ignore_ascii_case("[data-theme-id='']")
            {
                Some((css::AppearanceScope::Any, (0, 1, 0)))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn match_single_slot_selector(selector: &str) -> Option<SlotSelectorMatch> {
    let compounds = split_complex_selector(selector.trim());
    if compounds.is_empty() {
        return None;
    }

    let mut appearance = css::AppearanceScope::Any;
    let mut specificity: (u16, u16, u16) = (0, 0, 0);

    // Ancestor compounds (all except the rightmost subject compound).
    for &(comb, compound) in &compounds[..compounds.len() - 1] {
        if comb == Some(Combinator::Unsupported) {
            return None;
        }
        let tokens = tokenize_compound(compound)?;
        for token in &tokens {
            if let Some((app, spec)) = match_appearance_token(token) {
                appearance = combine_appearance(appearance, app)?;
                specificity = add_spec(specificity, spec);
            } else if match_slot_token(token).is_some() {
                specificity = add_spec(specificity, token_specificity(token));
            } else {
                match token {
                    SimpleToken::Universal => {}
                    SimpleToken::Type(t)
                        if t.eq_ignore_ascii_case("html") || t.eq_ignore_ascii_case("body") =>
                    {
                        specificity = add_spec(specificity, (0, 0, 1));
                    }
                    SimpleToken::PseudoClass(pc, None) if pc.eq_ignore_ascii_case("root") => {
                        specificity = add_spec(specificity, (0, 1, 0));
                    }
                    SimpleToken::Attr(name, _) if name.eq_ignore_ascii_case("data-theme-id") => {
                        specificity = add_spec(specificity, (0, 1, 0));
                    }
                    _ => return None,
                }
            }
        }
    }

    // Subject compound (rightmost).
    let (last_comb, subject) = compounds[compounds.len() - 1];
    if last_comb == Some(Combinator::Unsupported) {
        return None;
    }
    let tokens = tokenize_compound(subject)?;
    let mut matched_slot: Option<(ThemeSlot, Option<String>)> = None;
    let mut state = SlotState::Base;

    for token in &tokens {
        if let Some(found) = match_slot_token(token) {
            // If both a slot and ::-webkit-scrollbar appear on one compound (e.g.
            // `[data-slot="sidebar"]::-webkit-scrollbar`), the scrollbar pseudo-element is the subject.
            if matches!(
                found.0,
                ThemeSlot::ScrollAreaScrollbar | ThemeSlot::ScrollAreaThumb
            ) || matched_slot.is_none()
            {
                matched_slot = Some(found);
            } else {
                return None;
            }
            specificity = add_spec(specificity, token_specificity(token));
            continue;
        }
        if let Some((app, spec)) = match_appearance_token(token) {
            appearance = combine_appearance(appearance, app)?;
            specificity = add_spec(specificity, spec);
            continue;
        }
        match token {
            SimpleToken::Universal => {}
            SimpleToken::Type(_) => {
                specificity = add_spec(specificity, (0, 0, 1));
            }
            SimpleToken::PseudoClass(pc, None) if pc.eq_ignore_ascii_case("hover") => {
                if state != SlotState::Active {
                    state = SlotState::Hover;
                }
                specificity = add_spec(specificity, (0, 1, 0));
            }
            SimpleToken::PseudoClass(pc, None) if pc.eq_ignore_ascii_case("active") => {
                state = SlotState::Active;
                specificity = add_spec(specificity, (0, 1, 0));
            }
            SimpleToken::PseudoClass(pc, None) if pc.eq_ignore_ascii_case("root") => {
                specificity = add_spec(specificity, (0, 1, 0));
            }
            SimpleToken::Attr(name, _) if name.eq_ignore_ascii_case("data-theme-id") => {
                specificity = add_spec(specificity, (0, 1, 0));
            }
            _ => return None,
        }
    }

    let (slot, row_kind) = matched_slot?;
    Some(SlotSelectorMatch { slot, row_kind, state, appearance, specificity })
}

fn token_specificity(token: &SimpleToken<'_>) -> (u16, u16, u16) {
    match token {
        SimpleToken::Id => (1, 0, 0),
        SimpleToken::Class(_) | SimpleToken::Attr(_, _) | SimpleToken::PseudoClass(_, _) => {
            (0, 1, 0)
        }
        SimpleToken::Type(_) | SimpleToken::PseudoElement(_) => (0, 0, 1),
        SimpleToken::Universal => (0, 0, 0),
    }
}

// ---------------------------------------------------------------------------
// Compilation & Cascade
// ---------------------------------------------------------------------------

type CascadeKey = (bool, (u16, u16, u16), usize);

fn cascade_wins(existing: Option<CascadeKey>, candidate: CascadeKey) -> bool {
    match existing {
        None => true,
        Some(prev) => candidate >= prev,
    }
}

#[derive(Default)]
struct OverrideBuilder {
    display_none: Option<(CascadeKey, bool)>,
    bg_image: Option<(CascadeKey, Option<SlotBackground>)>,
    bg_color: Option<(CascadeKey, Option<SlotBackground>)>,
    bg_size: Option<(CascadeKey, BackgroundImageSize)>,
    color: Option<(CascadeKey, u32)>,
    border_color: Option<(CascadeKey, u32)>,
    border_top: Option<(CascadeKey, f32)>,
    border_right: Option<(CascadeKey, f32)>,
    border_bottom: Option<(CascadeKey, f32)>,
    border_left: Option<(CascadeKey, f32)>,
    radius_tl: Option<(CascadeKey, SlotLength)>,
    radius_tr: Option<(CascadeKey, SlotLength)>,
    radius_br: Option<(CascadeKey, SlotLength)>,
    radius_bl: Option<(CascadeKey, SlotLength)>,
    pad_top: Option<(CascadeKey, SlotLength)>,
    pad_right: Option<(CascadeKey, SlotLength)>,
    pad_bottom: Option<(CascadeKey, SlotLength)>,
    pad_left: Option<(CascadeKey, SlotLength)>,
    margin_top: Option<(CascadeKey, SlotLength)>,
    margin_right: Option<(CascadeKey, SlotLength)>,
    margin_bottom: Option<(CascadeKey, SlotLength)>,
    margin_left: Option<(CascadeKey, SlotLength)>,
    gap: Option<(CascadeKey, SlotLength)>,
    row_gap: Option<(CascadeKey, SlotLength)>,
    column_gap: Option<(CascadeKey, SlotLength)>,
    width: Option<(CascadeKey, SlotLength)>,
    min_width: Option<(CascadeKey, SlotLength)>,
    max_width: Option<(CascadeKey, SlotSizeConstraint)>,
    height: Option<(CascadeKey, SlotLength)>,
    min_height: Option<(CascadeKey, SlotLength)>,
    max_height: Option<(CascadeKey, SlotSizeConstraint)>,
    box_shadows: Option<(CascadeKey, Vec<SlotBoxShadow>)>,
    opacity: Option<(CascadeKey, f32)>,
    font_family: Option<(CascadeKey, String)>,
    font_size: Option<(CascadeKey, f32)>,
    font_weight: Option<(CascadeKey, u16)>,
    line_height: Option<(CascadeKey, SlotLineHeight)>,
    flex_direction: Option<(CascadeKey, SlotFlexDirection)>,
    align_items: Option<(CascadeKey, SlotAlignItems)>,
    justify_content: Option<(CascadeKey, SlotJustifyContent)>,
}

fn set_if_wins<T>(slot: &mut Option<(CascadeKey, T)>, key: CascadeKey, val: T) {
    if cascade_wins(slot.as_ref().map(|(k, _)| *k), key) {
        *slot = Some((key, val));
    }
}

impl OverrideBuilder {
    fn apply(&mut self, prop: &str, val: &str, key: CascadeKey, roles: &Colors, dark: bool) {
        let prop_lower = prop.to_ascii_lowercase();
        let val = val.trim();
        if val.is_empty() {
            return;
        }
        match prop_lower.as_str() {
            "display" => {
                let lower = val.to_ascii_lowercase();
                if lower == "none" {
                    set_if_wins(&mut self.display_none, key, true);
                } else if matches!(
                    lower.as_str(),
                    "flex" | "block" | "inline" | "inline-flex" | "grid" | "inline-block" | "contents"
                ) {
                    set_if_wins(&mut self.display_none, key, false);
                }
            }
            "background" => {
                if let Some((img, col, sz)) = parse_background_shorthand(val, roles, dark) {
                    set_if_wins(&mut self.bg_image, key, img);
                    set_if_wins(&mut self.bg_color, key, col);
                    if let Some(size) = sz {
                        set_if_wins(&mut self.bg_size, key, size);
                    }
                }
            }
            "background-color" => {
                if val.eq_ignore_ascii_case("transparent") || val.eq_ignore_ascii_case("none") {
                    set_if_wins(&mut self.bg_color, key, Some(SlotBackground::Transparent));
                } else if let Some(c) = eval_color(val, roles, dark) {
                    set_if_wins(&mut self.bg_color, key, Some(SlotBackground::Solid(c)));
                }
            }
            "background-image" => {
                if val.eq_ignore_ascii_case("none") {
                    set_if_wins(&mut self.bg_image, key, None);
                } else if let Some((bg, sz)) = parse_background_image(val, roles, dark) {
                    set_if_wins(&mut self.bg_image, key, Some(bg));
                    if let Some(size) = sz {
                        set_if_wins(&mut self.bg_size, key, size);
                    }
                }
            }
            "background-size" => {
                if let Some(sz) = parse_background_size(val) {
                    set_if_wins(&mut self.bg_size, key, sz);
                }
            }
            "color" => {
                if let Some(c) = eval_color(val, roles, dark) {
                    set_if_wins(&mut self.color, key, c);
                }
            }
            "border" => {
                if let Some((w, col)) = parse_border_shorthand(val, roles, dark) {
                    if let Some(width) = w {
                        set_if_wins(&mut self.border_top, key, width);
                        set_if_wins(&mut self.border_right, key, width);
                        set_if_wins(&mut self.border_bottom, key, width);
                        set_if_wins(&mut self.border_left, key, width);
                    }
                    if let Some(c) = col {
                        set_if_wins(&mut self.border_color, key, c);
                    }
                }
            }
            "border-top" => {
                if let Some((w, col)) = parse_border_shorthand(val, roles, dark) {
                    if let Some(width) = w {
                        set_if_wins(&mut self.border_top, key, width);
                    }
                    if let Some(c) = col {
                        set_if_wins(&mut self.border_color, key, c);
                    }
                }
            }
            "border-right" => {
                if let Some((w, col)) = parse_border_shorthand(val, roles, dark) {
                    if let Some(width) = w {
                        set_if_wins(&mut self.border_right, key, width);
                    }
                    if let Some(c) = col {
                        set_if_wins(&mut self.border_color, key, c);
                    }
                }
            }
            "border-bottom" => {
                if let Some((w, col)) = parse_border_shorthand(val, roles, dark) {
                    if let Some(width) = w {
                        set_if_wins(&mut self.border_bottom, key, width);
                    }
                    if let Some(c) = col {
                        set_if_wins(&mut self.border_color, key, c);
                    }
                }
            }
            "border-left" => {
                if let Some((w, col)) = parse_border_shorthand(val, roles, dark) {
                    if let Some(width) = w {
                        set_if_wins(&mut self.border_left, key, width);
                    }
                    if let Some(c) = col {
                        set_if_wins(&mut self.border_color, key, c);
                    }
                }
            }
            "border-width" => {
                if let Some([t, r, b, l]) = parse_edge_shorthand(val, parse_border_width) {
                    set_if_wins(&mut self.border_top, key, t);
                    set_if_wins(&mut self.border_right, key, r);
                    set_if_wins(&mut self.border_bottom, key, b);
                    set_if_wins(&mut self.border_left, key, l);
                }
            }
            "border-top-width" => {
                if let Some(w) = parse_border_width(val) {
                    set_if_wins(&mut self.border_top, key, w);
                }
            }
            "border-right-width" => {
                if let Some(w) = parse_border_width(val) {
                    set_if_wins(&mut self.border_right, key, w);
                }
            }
            "border-bottom-width" => {
                if let Some(w) = parse_border_width(val) {
                    set_if_wins(&mut self.border_bottom, key, w);
                }
            }
            "border-left-width" => {
                if let Some(w) = parse_border_width(val) {
                    set_if_wins(&mut self.border_left, key, w);
                }
            }
            "border-color"
            | "border-top-color"
            | "border-right-color"
            | "border-bottom-color"
            | "border-left-color" => {
                let first = split_ws_tokens(val).into_iter().next().unwrap_or(val);
                if let Some(c) = eval_color(first, roles, dark) {
                    set_if_wins(&mut self.border_color, key, c);
                }
            }
            "border-radius" => {
                let horiz = val.split('/').next().unwrap_or(val).trim();
                if let Some([tl, tr, br, bl]) = parse_corner_shorthand(horiz) {
                    set_if_wins(&mut self.radius_tl, key, tl);
                    set_if_wins(&mut self.radius_tr, key, tr);
                    set_if_wins(&mut self.radius_br, key, br);
                    set_if_wins(&mut self.radius_bl, key, bl);
                }
            }
            "border-top-left-radius" => {
                if let Some(r) = parse_radius_value(val) {
                    set_if_wins(&mut self.radius_tl, key, r);
                }
            }
            "border-top-right-radius" => {
                if let Some(r) = parse_radius_value(val) {
                    set_if_wins(&mut self.radius_tr, key, r);
                }
            }
            "border-bottom-right-radius" => {
                if let Some(r) = parse_radius_value(val) {
                    set_if_wins(&mut self.radius_br, key, r);
                }
            }
            "border-bottom-left-radius" => {
                if let Some(r) = parse_radius_value(val) {
                    set_if_wins(&mut self.radius_bl, key, r);
                }
            }
            "padding" => {
                if let Some([t, r, b, l]) = parse_edge_shorthand(val, parse_non_neg_definite_length) {
                    set_if_wins(&mut self.pad_top, key, t);
                    set_if_wins(&mut self.pad_right, key, r);
                    set_if_wins(&mut self.pad_bottom, key, b);
                    set_if_wins(&mut self.pad_left, key, l);
                }
            }
            "padding-top" | "padding-block-start" => {
                if let Some(v) = parse_non_neg_definite_length(val) {
                    set_if_wins(&mut self.pad_top, key, v);
                }
            }
            "padding-right" | "padding-inline-end" => {
                if let Some(v) = parse_non_neg_definite_length(val) {
                    set_if_wins(&mut self.pad_right, key, v);
                }
            }
            "padding-bottom" | "padding-block-end" => {
                if let Some(v) = parse_non_neg_definite_length(val) {
                    set_if_wins(&mut self.pad_bottom, key, v);
                }
            }
            "padding-left" | "padding-inline-start" => {
                if let Some(v) = parse_non_neg_definite_length(val) {
                    set_if_wins(&mut self.pad_left, key, v);
                }
            }
            "padding-inline" => {
                if let Some((a, b)) = parse_two_value_shorthand(val, parse_non_neg_definite_length) {
                    set_if_wins(&mut self.pad_left, key, a);
                    set_if_wins(&mut self.pad_right, key, b);
                }
            }
            "padding-block" => {
                if let Some((a, b)) = parse_two_value_shorthand(val, parse_non_neg_definite_length) {
                    set_if_wins(&mut self.pad_top, key, a);
                    set_if_wins(&mut self.pad_bottom, key, b);
                }
            }
            "margin" => {
                if let Some([t, r, b, l]) = parse_edge_shorthand(val, parse_margin_length) {
                    set_if_wins(&mut self.margin_top, key, t);
                    set_if_wins(&mut self.margin_right, key, r);
                    set_if_wins(&mut self.margin_bottom, key, b);
                    set_if_wins(&mut self.margin_left, key, l);
                }
            }
            "margin-top" | "margin-block-start" => {
                if let Some(v) = parse_margin_length(val) {
                    set_if_wins(&mut self.margin_top, key, v);
                }
            }
            "margin-right" | "margin-inline-end" => {
                if let Some(v) = parse_margin_length(val) {
                    set_if_wins(&mut self.margin_right, key, v);
                }
            }
            "margin-bottom" | "margin-block-end" => {
                if let Some(v) = parse_margin_length(val) {
                    set_if_wins(&mut self.margin_bottom, key, v);
                }
            }
            "margin-left" | "margin-inline-start" => {
                if let Some(v) = parse_margin_length(val) {
                    set_if_wins(&mut self.margin_left, key, v);
                }
            }
            "margin-inline" => {
                if let Some((a, b)) = parse_two_value_shorthand(val, parse_margin_length) {
                    set_if_wins(&mut self.margin_left, key, a);
                    set_if_wins(&mut self.margin_right, key, b);
                }
            }
            "margin-block" => {
                if let Some((a, b)) = parse_two_value_shorthand(val, parse_margin_length) {
                    set_if_wins(&mut self.margin_top, key, a);
                    set_if_wins(&mut self.margin_bottom, key, b);
                }
            }
            "gap" => {
                if let Some((row, col)) = parse_two_value_shorthand(val, parse_non_neg_definite_length) {
                    if row == col {
                        set_if_wins(&mut self.gap, key, row);
                    }
                    set_if_wins(&mut self.row_gap, key, row);
                    set_if_wins(&mut self.column_gap, key, col);
                }
            }
            "row-gap" => {
                if let Some(v) = parse_non_neg_definite_length(val) {
                    set_if_wins(&mut self.row_gap, key, v);
                }
            }
            "column-gap" => {
                if let Some(v) = parse_non_neg_definite_length(val) {
                    set_if_wins(&mut self.column_gap, key, v);
                }
            }
            "width" => {
                if let Some(v) = parse_dimension_length(val) {
                    set_if_wins(&mut self.width, key, v);
                }
            }
            "min-width" => {
                if let Some(v) = parse_dimension_length(val) {
                    set_if_wins(&mut self.min_width, key, v);
                }
            }
            "max-width" => {
                if let Some(v) = parse_size_constraint(val) {
                    set_if_wins(&mut self.max_width, key, v);
                }
            }
            "height" => {
                if let Some(v) = parse_dimension_length(val) {
                    set_if_wins(&mut self.height, key, v);
                }
            }
            "min-height" => {
                if let Some(v) = parse_dimension_length(val) {
                    set_if_wins(&mut self.min_height, key, v);
                }
            }
            "max-height" => {
                if let Some(v) = parse_size_constraint(val) {
                    set_if_wins(&mut self.max_height, key, v);
                }
            }
            "box-shadow" => {
                if let Some(shadows) = parse_box_shadows(val, roles, dark) {
                    set_if_wins(&mut self.box_shadows, key, shadows);
                }
            }
            "opacity" => {
                if let Some(op) = parse_opacity(val) {
                    set_if_wins(&mut self.opacity, key, op);
                }
            }
            "font-family" => {
                let cleaned = val.trim();
                if !cleaned.is_empty() && !cleaned.eq_ignore_ascii_case("inherit") {
                    set_if_wins(&mut self.font_family, key, cleaned.to_owned());
                }
            }
            "font-size" => {
                if let Some(sz) = parse_font_size(val) {
                    set_if_wins(&mut self.font_size, key, sz);
                }
            }
            "font-weight" => {
                if let Some(w) = parse_font_weight(val) {
                    set_if_wins(&mut self.font_weight, key, w);
                }
            }
            "line-height" => {
                if let Some(lh) = parse_line_height(val) {
                    set_if_wins(&mut self.line_height, key, lh);
                }
            }
            "flex-direction" => {
                let dir = match val.to_ascii_lowercase().as_str() {
                    "row" => Some(SlotFlexDirection::Row),
                    "column" => Some(SlotFlexDirection::Column),
                    "row-reverse" => Some(SlotFlexDirection::RowReverse),
                    "column-reverse" => Some(SlotFlexDirection::ColumnReverse),
                    _ => None,
                };
                if let Some(dir) = dir {
                    set_if_wins(&mut self.flex_direction, key, dir);
                }
            }
            "align-items" => {
                let align = match val.to_ascii_lowercase().as_str() {
                    "start" => Some(SlotAlignItems::Start),
                    "end" => Some(SlotAlignItems::End),
                    "flex-start" => Some(SlotAlignItems::FlexStart),
                    "flex-end" => Some(SlotAlignItems::FlexEnd),
                    "center" => Some(SlotAlignItems::Center),
                    "baseline" => Some(SlotAlignItems::Baseline),
                    "stretch" => Some(SlotAlignItems::Stretch),
                    _ => None,
                };
                if let Some(align) = align {
                    set_if_wins(&mut self.align_items, key, align);
                }
            }
            "justify-content" => {
                let justify = match val.to_ascii_lowercase().as_str() {
                    "start" => Some(SlotJustifyContent::Start),
                    "end" => Some(SlotJustifyContent::End),
                    "flex-start" => Some(SlotJustifyContent::FlexStart),
                    "flex-end" => Some(SlotJustifyContent::FlexEnd),
                    "center" => Some(SlotJustifyContent::Center),
                    "stretch" => Some(SlotJustifyContent::Stretch),
                    "space-between" => Some(SlotJustifyContent::SpaceBetween),
                    "space-around" => Some(SlotJustifyContent::SpaceAround),
                    "space-evenly" => Some(SlotJustifyContent::SpaceEvenly),
                    _ => None,
                };
                if let Some(justify) = justify {
                    set_if_wins(&mut self.justify_content, key, justify);
                }
            }
            _ => {}
        }
    }

    fn finish(self) -> SlotStyleOverride {
        let background = match (self.bg_image, self.bg_color) {
            (
                Some((img_key, Some(SlotBackground::ThemeBackgroundImage { mut size, mut overlay_color }))),
                col_entry,
            ) => {
                if let Some((sz_key, sz)) = self.bg_size
                    && sz_key >= img_key
                {
                    size = sz;
                }
                if overlay_color.is_none()
                    && let Some((_, Some(SlotBackground::Solid(c)))) = col_entry
                {
                    overlay_color = Some(c);
                }
                Some(SlotBackground::ThemeBackgroundImage { size, overlay_color })
            }
            (Some((_, Some(img))), _) => Some(img),
            (_, Some((_, col))) => col,
            _ => None,
        };

        SlotStyleOverride {
            display_none: self.display_none.map(|(_, v)| v),
            background,
            color: self.color.map(|(_, v)| v),
            border_color: self.border_color.map(|(_, v)| v),
            border_widths: SlotEdges {
                top: self.border_top.map(|(_, v)| v),
                right: self.border_right.map(|(_, v)| v),
                bottom: self.border_bottom.map(|(_, v)| v),
                left: self.border_left.map(|(_, v)| v),
            },
            border_radii: SlotCorners {
                top_left: self.radius_tl.map(|(_, v)| v),
                top_right: self.radius_tr.map(|(_, v)| v),
                bottom_right: self.radius_br.map(|(_, v)| v),
                bottom_left: self.radius_bl.map(|(_, v)| v),
            },
            padding: SlotEdges {
                top: self.pad_top.map(|(_, v)| v),
                right: self.pad_right.map(|(_, v)| v),
                bottom: self.pad_bottom.map(|(_, v)| v),
                left: self.pad_left.map(|(_, v)| v),
            },
            margin: SlotEdges {
                top: self.margin_top.map(|(_, v)| v),
                right: self.margin_right.map(|(_, v)| v),
                bottom: self.margin_bottom.map(|(_, v)| v),
                left: self.margin_left.map(|(_, v)| v),
            },
            gap: self.gap.map(|(_, v)| v),
            row_gap: self.row_gap.map(|(_, v)| v),
            column_gap: self.column_gap.map(|(_, v)| v),
            width: self.width.map(|(_, v)| v),
            min_width: self.min_width.map(|(_, v)| v),
            max_width: self.max_width.map(|(_, v)| v),
            height: self.height.map(|(_, v)| v),
            min_height: self.min_height.map(|(_, v)| v),
            max_height: self.max_height.map(|(_, v)| v),
            box_shadows: self.box_shadows.map(|(_, v)| v),
            opacity: self.opacity.map(|(_, v)| v),
            font_family: self.font_family.map(|(_, v)| v),
            font_size: self.font_size.map(|(_, v)| v),
            font_weight: self.font_weight.map(|(_, v)| v),
            line_height: self.line_height.map(|(_, v)| v),
            flex_direction: self.flex_direction.map(|(_, v)| v),
            align_items: self.align_items.map(|(_, v)| v),
            justify_content: self.justify_content.map(|(_, v)| v),
        }
    }
}

pub(crate) fn compile_slot_stylesheet(
    slot_decls: &[SlotDecl],
    slot_vars: &[SlotVarDecl],
    root_vars: &BTreeMap<String, String>,
    dark: bool,
    roles: &Colors,
    palette: &BTreeMap<String, u32>,
) -> SlotStylesheet {
    if slot_decls.is_empty() {
        return SlotStylesheet::empty();
    }

    let base_vars = build_base_vars(root_vars, roles, palette);
    let mut slots: Box<[Option<SlotStyleSet>; ThemeSlot::COUNT]> =
        Box::new(std::array::from_fn(|_| None));
    let mut timeline_kinds: BTreeSet<String> = BTreeSet::new();

    for decl in slot_decls {
        if decl.slot == ThemeSlot::TimelineRow
            && decl.appearance.matches(dark)
            && let Some(kind) = &decl.row_kind
        {
            timeline_kinds.insert(kind.clone());
        }
    }

    let mut any_non_empty = false;
    let mut has_scoped_bg_image = false;

    for slot in ThemeSlot::ALL {
        let set = compile_slot_set(slot, None, slot_decls, slot_vars, &base_vars, dark, roles);
        if !set.is_empty() {
            any_non_empty = true;
            if set.has_scoped_background_image() {
                has_scoped_bg_image = true;
            }
            slots[slot.index()] = Some(set);
        }
    }

    let mut timeline_rows = BTreeMap::new();
    for kind in timeline_kinds {
        let set = compile_slot_set(
            ThemeSlot::TimelineRow,
            Some(kind.as_str()),
            slot_decls,
            slot_vars,
            &base_vars,
            dark,
            roles,
        );
        if !set.is_empty() {
            any_non_empty = true;
            if set.has_scoped_background_image() {
                has_scoped_bg_image = true;
            }
            timeline_rows.insert(kind, set);
        }
    }

    SlotStylesheet {
        slots,
        timeline_rows,
        empty: !any_non_empty,
        has_scoped_bg_image,
    }
}

fn compile_slot_set(
    slot: ThemeSlot,
    row_kind: Option<&str>,
    slot_decls: &[SlotDecl],
    slot_vars: &[SlotVarDecl],
    base_vars: &BTreeMap<String, String>,
    dark: bool,
    roles: &Colors,
) -> SlotStyleSet {
    let mut vars = base_vars.clone();
    let mut local_best: BTreeMap<&str, (CascadeKey, &str)> = BTreeMap::new();
    for decl in slot_vars {
        if decl.slot != slot || !decl.appearance.matches(dark) {
            continue;
        }
        let kind_matches = match (row_kind, decl.row_kind.as_deref()) {
            (None, None) => true,
            (Some(target), None) => !target.is_empty(),
            (Some(target), Some(dk)) => target == dk,
            (None, Some(_)) => false,
        };
        if !kind_matches {
            continue;
        }
        let key = (decl.important, decl.specificity, decl.order);
        if cascade_wins(local_best.get(decl.name.as_str()).map(|(k, _)| *k), key) {
            local_best.insert(decl.name.as_str(), (key, decl.value.as_str()));
        }
    }
    for (name, (_, val)) in local_best {
        if !name.eq_ignore_ascii_case("--theme-background-image") {
            vars.insert(name.to_owned(), val.to_owned());
        }
    }

    let mut base = OverrideBuilder::default();
    let mut hover = OverrideBuilder::default();
    let mut active = OverrideBuilder::default();

    for decl in slot_decls {
        if decl.slot != slot || !decl.appearance.matches(dark) {
            continue;
        }
        let kind_matches = match (row_kind, decl.row_kind.as_deref()) {
            (None, None) => true,
            (Some(target), None) => !target.is_empty(),
            (Some(target), Some(dk)) => target == dk,
            (None, Some(_)) => false,
        };
        if !kind_matches {
            continue;
        }
        let resolved = css::resolve_var_expr(&decl.value, &vars, &mut BTreeSet::new());
        if css::value_fetches(&resolved) {
            continue;
        }
        let key = (decl.important, decl.specificity, decl.order);
        let builder = match decl.state {
            SlotState::Base => &mut base,
            SlotState::Hover => &mut hover,
            SlotState::Active => &mut active,
        };
        builder.apply(&decl.property, &resolved, key, roles, dark);
    }

    SlotStyleSet {
        base: base.finish(),
        hover: hover.finish(),
        active: active.finish(),
    }
}

fn build_base_vars(
    root_vars: &BTreeMap<String, String>,
    roles: &Colors,
    palette: &BTreeMap<String, u32>,
) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    // Seed semantic palette tokens (`--background`, `--primary`, `--border`, etc.).
    for (token, packed) in palette {
        let hex = format!("#{packed:08x}");
        let css_name = format!("--{}", token.replace('_', "-"));
        vars.insert(css_name, hex);
    }
    // Seed `--app-theme-<kebab-role>` and direct role names.
    for (role, value) in roles {
        let kebab = camel_to_kebab(role);
        vars.insert(format!("--app-theme-{kebab}"), value.clone());
        vars.entry(format!("--{kebab}")).or_insert_with(|| value.clone());
    }
    // User CSS `:root` / mode custom properties take precedence, except we preserve
    // `--theme-background-image` as an unresolved sentinel for background image scoping.
    for (name, value) in root_vars {
        if !name.eq_ignore_ascii_case("--theme-background-image") {
            vars.insert(name.clone(), value.clone());
        }
    }
    vars
}

fn camel_to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        if ch.is_ascii_uppercase() {
            out.push('-');
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Value parsers & safety clamps
// ---------------------------------------------------------------------------

fn eval_color(raw: &str, roles: &Colors, dark: bool) -> Option<u32> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // Guard against malformed functional forms that `color::evaluate` indexes directly.
    if let Some(args) = s.strip_prefix("light-dark(").and_then(|s| s.strip_suffix(')'))
        && color::split(args, ',').len() != 2
    {
        return None;
    }
    if let Some(args) = s.strip_prefix("--alpha(").and_then(|s| s.strip_suffix(')'))
        && color::split(args, '/').len() != 2
    {
        return None;
    }
    if let Some(args) = s.strip_prefix("color-mix(").and_then(|s| s.strip_suffix(')'))
        && color::split(args, ',').len() != 3
    {
        return None;
    }
    color::evaluate(s, roles, dark).ok().map(color::packed)
}

fn split_ws_tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        let mut depth = 0usize;
        let mut quote: Option<u8> = None;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = quote {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            } else {
                match b {
                    b'"' | b'\'' => quote = Some(b),
                    b'(' => depth += 1,
                    b')' => depth = depth.saturating_sub(1),
                    b' ' | b'\t' | b'\n' | b'\r' if depth == 0 => break,
                    _ => {}
                }
            }
            i += 1;
        }
        out.push(&s[start..i]);
    }
    out
}

fn contains_theme_bg_var(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let compact: String = lower.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    compact.contains("var(--theme-background-image")
}

fn parse_background_size(val: &str) -> Option<BackgroundImageSize> {
    match val.trim().to_ascii_lowercase().as_str() {
        "cover" => Some(BackgroundImageSize::Cover),
        "contain" => Some(BackgroundImageSize::Contain),
        "auto" => Some(BackgroundImageSize::Auto),
        _ => None,
    }
}

fn parse_background_shorthand(
    val: &str,
    roles: &Colors,
    dark: bool,
) -> Option<(Option<SlotBackground>, Option<SlotBackground>, Option<BackgroundImageSize>)> {
    if val.eq_ignore_ascii_case("transparent") || val.eq_ignore_ascii_case("none") {
        return Some((None, Some(SlotBackground::Transparent), None));
    }
    if contains_theme_bg_var(val) {
        let (bg, sz) = parse_theme_bg_layers(val, roles, dark);
        return Some((Some(bg), None, sz));
    }
    if let Some(grad) = parse_linear_gradient(val, roles, dark) {
        return Some((Some(grad), None, None));
    }
    if let Some(c) = eval_color(val, roles, dark) {
        return Some((None, Some(SlotBackground::Solid(c)), None));
    }
    // Check comma-separated layers and shorthand tokens (e.g. `linear-gradient(...) no-repeat`
    // or `linear-gradient(...), #111111`).
    let mut found_grad: Option<SlotBackground> = None;
    let mut found_color: Option<u32> = None;
    for layer in color::split(val, ',') {
        let layer = layer.trim();
        if found_grad.is_none()
            && let Some(grad) = parse_linear_gradient(layer, roles, dark)
        {
            found_grad = Some(grad);
            continue;
        }
        for token in split_ws_tokens(layer) {
            if found_grad.is_none()
                && let Some(grad) = parse_linear_gradient(token, roles, dark)
            {
                found_grad = Some(grad);
            } else if found_color.is_none()
                && let Some(c) = eval_color(token, roles, dark)
            {
                found_color = Some(c);
            }
        }
    }
    if found_grad.is_some() || found_color.is_some() {
        return Some((found_grad, found_color.map(SlotBackground::Solid), None));
    }
    None
}

fn parse_background_image(
    val: &str,
    roles: &Colors,
    dark: bool,
) -> Option<(SlotBackground, Option<BackgroundImageSize>)> {
    if contains_theme_bg_var(val) {
        return Some(parse_theme_bg_layers(val, roles, dark));
    }
    for layer in color::split(val, ',') {
        if let Some(grad) = parse_linear_gradient(layer.trim(), roles, dark) {
            return Some((grad, None));
        }
    }
    None
}

fn parse_theme_bg_layers(
    val: &str,
    roles: &Colors,
    dark: bool,
) -> (SlotBackground, Option<BackgroundImageSize>) {
    let mut size = BackgroundImageSize::Cover;
    let mut explicit_size = None;
    let mut overlay_color = None;

    for layer in color::split(val, ',') {
        let layer = layer.trim();
        let lower = layer.to_ascii_lowercase();
        if lower.starts_with("linear-gradient(") {
            if let Some(SlotBackground::LinearGradient { stops, .. }) =
                parse_linear_gradient(layer, roles, dark)
                && let Some(first) = stops.first()
            {
                overlay_color = Some(first.color);
            }
            continue;
        }
        // Parse tokens around `var(--theme-background-image)` for size (`/ cover`, `contain`, `auto`) and color.
        let normalized = layer.replace('/', " / ");
        for tok in split_ws_tokens(&normalized) {
            match tok.to_ascii_lowercase().as_str() {
                "cover" => {
                    size = BackgroundImageSize::Cover;
                    explicit_size = Some(size);
                }
                "contain" => {
                    size = BackgroundImageSize::Contain;
                    explicit_size = Some(size);
                }
                "auto" => {
                    size = BackgroundImageSize::Auto;
                    explicit_size = Some(size);
                }
                "/" | "center" | "top" | "bottom" | "left" | "right" | "no-repeat" | "repeat"
                | "repeat-x" | "repeat-y" | "fixed" | "scroll" => {}
                other if other.starts_with("var(") => {}
                _ => {
                    if overlay_color.is_none()
                        && let Some(c) = eval_color(tok, roles, dark)
                    {
                        overlay_color = Some(c);
                    }
                }
            }
        }
    }

    (SlotBackground::ThemeBackgroundImage { size, overlay_color }, explicit_size)
}

fn parse_linear_gradient(val: &str, roles: &Colors, dark: bool) -> Option<SlotBackground> {
    let trimmed = val.trim();
    if trimmed.len() < 17
        || !trimmed[..16].eq_ignore_ascii_case("linear-gradient(")
        || !trimmed.ends_with(')')
    {
        return None;
    }
    let inner = &trimmed[16..trimmed.len() - 1];
    let parts = color::split(inner, ',');
    if parts.len() < 2 {
        return None;
    }

    let (angle_deg, stop_slice) = if let Some(angle) = parse_gradient_direction(parts[0]) {
        (angle, &parts[1..])
    } else {
        (180.0, &parts[..])
    };
    if stop_slice.len() < 2 {
        return None;
    }

    let mut stops = Vec::with_capacity(stop_slice.len());
    for part in stop_slice {
        let tokens = split_ws_tokens(part);
        if tokens.is_empty() {
            return None;
        }
        let (color_tok, pos) = if tokens.len() >= 2
            && let Some(p) = parse_percentage_unit(tokens[tokens.len() - 1])
        {
            (tokens[..tokens.len() - 1].join(" "), Some(p.clamp(0.0, 1.0)))
        } else {
            (part.trim().to_owned(), None)
        };
        let packed = eval_color(&color_tok, roles, dark)?;
        stops.push(GradientStop { color: packed, position: pos });
    }

    // Fill implicit stop positions (0.0 -> 1.0 evenly).
    let last_idx = stops.len() - 1;
    if stops[0].position.is_none() {
        stops[0].position = Some(0.0);
    }
    if stops[last_idx].position.is_none() {
        stops[last_idx].position = Some(1.0);
    }
    let mut i = 1;
    while i < last_idx {
        if stops[i].position.is_none() {
            let start_pos = stops[i - 1].position.unwrap_or(0.0);
            let mut end = i + 1;
            while end < last_idx && stops[end].position.is_none() {
                end += 1;
            }
            let end_pos = stops[end].position.unwrap_or(1.0);
            let steps = (end - (i - 1)) as f32;
            for (k, stop) in stops[i..end].iter_mut().enumerate() {
                stop.position = Some(start_pos + (end_pos - start_pos) * ((k + 1) as f32 / steps));
            }
            i = end;
        } else {
            i += 1;
        }
    }

    Some(SlotBackground::LinearGradient { angle_deg, stops })
}

fn parse_gradient_direction(raw: &str) -> Option<f32> {
    let lower = raw.trim().to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("to ") {
        let mut words: Vec<&str> = rest.split_whitespace().collect();
        words.sort_unstable();
        return match words.as_slice() {
            ["top"] => Some(0.0),
            ["right", "top"] => Some(45.0),
            ["right"] => Some(90.0),
            ["bottom", "right"] => Some(135.0),
            ["bottom"] => Some(180.0),
            ["bottom", "left"] => Some(225.0),
            ["left"] => Some(270.0),
            ["left", "top"] => Some(315.0),
            _ => None,
        };
    }
    if let Some(n) = lower.strip_suffix("deg") {
        return parse_finite_f32(n).map(|a| a.rem_euclid(360.0));
    }
    if let Some(n) = lower.strip_suffix("turn") {
        return parse_finite_f32(n).map(|a| (a * 360.0).rem_euclid(360.0));
    }
    if let Some(n) = lower.strip_suffix("rad") {
        return parse_finite_f32(n).map(|a| a.to_degrees().rem_euclid(360.0));
    }
    if let Some(n) = lower.strip_suffix("grad") {
        return parse_finite_f32(n).map(|a| (a * 0.9).rem_euclid(360.0));
    }
    None
}

fn parse_border_shorthand(val: &str, roles: &Colors, dark: bool) -> Option<(Option<f32>, Option<u32>)> {
    let trimmed = val.trim();
    if trimmed.eq_ignore_ascii_case("none") || trimmed == "0" {
        return Some((Some(0.0), None));
    }
    let mut width: Option<f32> = None;
    let mut color_val: Option<u32> = None;
    let mut saw_none_style = false;
    let mut saw_valid_token = false;

    for token in split_ws_tokens(trimmed) {
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "none" | "hidden" => {
                saw_none_style = true;
                saw_valid_token = true;
            }
            "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset" => {
                saw_valid_token = true;
            }
            _ => {
                if width.is_none()
                    && let Some(w) = parse_border_width(token)
                {
                    width = Some(w);
                    saw_valid_token = true;
                } else if color_val.is_none()
                    && let Some(c) = eval_color(token, roles, dark)
                {
                    color_val = Some(c);
                    saw_valid_token = true;
                } else {
                    return None;
                }
            }
        }
    }

    if !saw_valid_token {
        return None;
    }
    if saw_none_style {
        width = Some(0.0);
    }
    Some((width, color_val))
}

fn parse_border_width(val: &str) -> Option<f32> {
    let lower = val.trim().to_ascii_lowercase();
    let px = match lower.as_str() {
        "thin" => 1.0,
        "medium" => 3.0,
        "thick" => 5.0,
        "0" => 0.0,
        _ => parse_px_or_rem(&lower)?,
    };
    if px < 0.0 {
        return None;
    }
    Some(px.clamp(0.0, MAX_BORDER_WIDTH_PX))
}

fn parse_radius_value(val: &str) -> Option<SlotLength> {
    let lower = val.trim().to_ascii_lowercase();
    if lower == "0" {
        return Some(SlotLength::Px(0.0));
    }
    if let Some(pct) = parse_percentage_unit(&lower) {
        if pct < 0.0 {
            return None;
        }
        if pct >= 0.5 {
            return Some(SlotLength::Px(PILL_RADIUS_PX));
        }
        return Some(SlotLength::Fraction(pct));
    }
    if let Some(n) = lower.strip_suffix("px") {
        let v = parse_finite_f32(n)?;
        if v < 0.0 {
            return None;
        }
        return Some(SlotLength::Px(v.min(PILL_RADIUS_PX)));
    }
    if let Some(n) = lower.strip_suffix("rem") {
        let v = parse_finite_f32(n)?;
        if v < 0.0 {
            return None;
        }
        return Some(SlotLength::Rem(v.min(PILL_RADIUS_PX / ROOT_REM_PX)));
    }
    None
}

fn parse_corner_shorthand(val: &str) -> Option<[SlotLength; 4]> {
    let tokens: Vec<SlotLength> = val
        .split_whitespace()
        .map(parse_radius_value)
        .collect::<Option<Vec<_>>>()?;
    match tokens.as_slice() {
        [a] => Some([*a, *a, *a, *a]),
        [a, b] => Some([*a, *b, *a, *b]),
        [a, b, c] => Some([*a, *b, *c, *b]),
        [a, b, c, d] => Some([*a, *b, *c, *d]),
        _ => None,
    }
}

fn parse_edge_shorthand<T: Copy>(val: &str, parse_one: fn(&str) -> Option<T>) -> Option<[T; 4]> {
    let tokens: Vec<T> = val
        .split_whitespace()
        .map(parse_one)
        .collect::<Option<Vec<_>>>()?;
    match tokens.as_slice() {
        [a] => Some([*a, *a, *a, *a]),
        [v, h] => Some([*v, *h, *v, *h]),
        [t, h, b] => Some([*t, *h, *b, *h]),
        [t, r, b, l] => Some([*t, *r, *b, *l]),
        _ => None,
    }
}

fn parse_two_value_shorthand<T: Copy>(val: &str, parse_one: fn(&str) -> Option<T>) -> Option<(T, T)> {
    let tokens: Vec<T> = val
        .split_whitespace()
        .map(parse_one)
        .collect::<Option<Vec<_>>>()?;
    match tokens.as_slice() {
        [a] => Some((*a, *a)),
        [a, b] => Some((*a, *b)),
        _ => None,
    }
}

fn parse_non_neg_definite_length(val: &str) -> Option<SlotLength> {
    let len = parse_length(val, false, false)?;
    match len {
        SlotLength::Px(v) | SlotLength::Rem(v) | SlotLength::Fraction(v) if v < 0.0 => None,
        other => Some(other),
    }
}

fn parse_margin_length(val: &str) -> Option<SlotLength> {
    parse_length(val, true, true)
}

fn parse_dimension_length(val: &str) -> Option<SlotLength> {
    parse_length(val, false, true)
}

fn parse_length(val: &str, allow_negative: bool, allow_auto: bool) -> Option<SlotLength> {
    let lower = val.trim().to_ascii_lowercase();
    if lower == "auto" {
        return allow_auto.then_some(SlotLength::Auto);
    }
    if lower == "0" {
        return Some(SlotLength::Px(0.0));
    }
    if let Some(n) = lower.strip_suffix("px") {
        let v = parse_finite_f32(n)?;
        if !allow_negative && v < 0.0 {
            return None;
        }
        return Some(SlotLength::Px(v.clamp(-10_000.0, 10_000.0)));
    }
    if let Some(n) = lower.strip_suffix("rem") {
        let v = parse_finite_f32(n)?;
        if !allow_negative && v < 0.0 {
            return None;
        }
        return Some(SlotLength::Rem(v.clamp(-1000.0, 1000.0)));
    }
    if let Some(f) = parse_percentage_unit(&lower) {
        if !allow_negative && f < 0.0 {
            return None;
        }
        return Some(SlotLength::Fraction(f.clamp(-10.0, 10.0)));
    }
    None
}

fn parse_size_constraint(val: &str) -> Option<SlotSizeConstraint> {
    let lower = val.trim().to_ascii_lowercase();
    if lower == "none" {
        return Some(SlotSizeConstraint::None);
    }
    match parse_length(&lower, false, false)? {
        SlotLength::Px(v) => Some(SlotSizeConstraint::Px(v)),
        SlotLength::Rem(v) => Some(SlotSizeConstraint::Rem(v)),
        SlotLength::Fraction(v) => Some(SlotSizeConstraint::Fraction(v)),
        SlotLength::Auto => None,
    }
}

fn parse_box_shadows(val: &str, roles: &Colors, dark: bool) -> Option<Vec<SlotBoxShadow>> {
    let trimmed = val.trim();
    if trimmed.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }
    let mut shadows = Vec::new();
    for layer in color::split(trimmed, ',') {
        let tokens = split_ws_tokens(layer);
        if tokens.is_empty() {
            return None;
        }
        let mut inset = false;
        let mut lengths: Vec<f32> = Vec::new();
        let mut shadow_color: Option<u32> = None;

        for tok in tokens {
            if tok.eq_ignore_ascii_case("inset") {
                if inset {
                    return None;
                }
                inset = true;
            } else if let Some(px) = parse_px_or_rem(tok) {
                if lengths.len() >= 4 {
                    return None;
                }
                lengths.push(px);
            } else if shadow_color.is_none()
                && let Some(c) = eval_color(tok, roles, dark)
            {
                shadow_color = Some(c);
            } else {
                return None;
            }
        }
        if lengths.len() < 2 {
            return None;
        }
        let blur = if lengths.len() >= 3 {
            if lengths[2] < 0.0 {
                return None;
            }
            lengths[2].clamp(0.0, MAX_BLUR_PX)
        } else {
            0.0
        };
        let spread = if lengths.len() >= 4 {
            lengths[3].clamp(-128.0, 128.0)
        } else {
            0.0
        };
        shadows.push(SlotBoxShadow {
            offset_x: lengths[0].clamp(-MAX_BLUR_PX, MAX_BLUR_PX),
            offset_y: lengths[1].clamp(-MAX_BLUR_PX, MAX_BLUR_PX),
            blur_radius: blur,
            spread_radius: spread,
            color: shadow_color.unwrap_or(0x0000_00ff),
            inset,
        });
    }
    if shadows.is_empty() { None } else { Some(shadows) }
}

fn parse_opacity(val: &str) -> Option<f32> {
    let trimmed = val.trim();
    if let Some(pct) = parse_percentage_unit(trimmed) {
        return Some(pct.clamp(0.0, 1.0));
    }
    parse_finite_f32(trimmed).map(|v| v.clamp(0.0, 1.0))
}

fn parse_font_size(val: &str) -> Option<f32> {
    let lower = val.trim().to_ascii_lowercase();
    let px = parse_px_or_rem(&lower)?;
    if px <= 0.0 {
        return None;
    }
    Some(px.clamp(MIN_FONT_SIZE_PX, MAX_FONT_SIZE_PX))
}

fn parse_font_weight(val: &str) -> Option<u16> {
    let lower = val.trim().to_ascii_lowercase();
    match lower.as_str() {
        "thin" => Some(100),
        "extralight" | "ultralight" => Some(200),
        "light" => Some(300),
        "normal" | "regular" => Some(400),
        "medium" => Some(500),
        "semibold" | "demibold" => Some(600),
        "bold" => Some(700),
        "extrabold" | "ultrabold" => Some(800),
        "black" | "heavy" => Some(900),
        _ => parse_finite_f32(&lower)
            .filter(|n| *n >= 1.0)
            .map(|n| n.round().clamp(100.0, 900.0) as u16),
    }
}

fn parse_line_height(val: &str) -> Option<SlotLineHeight> {
    let lower = val.trim().to_ascii_lowercase();
    if lower == "normal" {
        return Some(SlotLineHeight::Relative(1.4));
    }
    if let Some(n) = lower.strip_suffix("px") {
        let v = parse_finite_f32(n)?;
        if v <= 0.0 {
            return None;
        }
        return Some(SlotLineHeight::Px(v.clamp(MIN_FONT_SIZE_PX, 160.0)));
    }
    if let Some(n) = lower.strip_suffix("rem") {
        let v = parse_finite_f32(n)?;
        if v <= 0.0 {
            return None;
        }
        return Some(SlotLineHeight::Rem(v.clamp(0.5, 10.0)));
    }
    if let Some(pct) = parse_percentage_unit(&lower) {
        if pct <= 0.0 {
            return None;
        }
        return Some(SlotLineHeight::Relative(pct.clamp(0.5, 4.0)));
    }
    let ratio = parse_finite_f32(&lower)?;
    if ratio <= 0.0 {
        return None;
    }
    Some(SlotLineHeight::Relative(ratio.clamp(0.5, 4.0)))
}

fn parse_px_or_rem(val: &str) -> Option<f32> {
    let lower = val.trim().to_ascii_lowercase();
    if lower == "0" {
        return Some(0.0);
    }
    if let Some(n) = lower.strip_suffix("px") {
        return parse_finite_f32(n);
    }
    if let Some(n) = lower.strip_suffix("rem") {
        return parse_finite_f32(n).map(|r| r * ROOT_REM_PX);
    }
    None
}

fn parse_percentage_unit(val: &str) -> Option<f32> {
    val.trim()
        .strip_suffix('%')
        .and_then(parse_finite_f32)
        .map(|n| n / 100.0)
}

fn parse_finite_f32(s: &str) -> Option<f32> {
    let n = s.trim().parse::<f32>().ok()?;
    n.is_finite().then_some(n)
}
