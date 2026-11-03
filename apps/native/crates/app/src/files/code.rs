//! The source view: one list row per line with Pierre's file layout (`ReadOnlySourcePreview.tsx`
//! with `FILE_LINK_REVEAL_UNSAFE_CSS`): 13px mono on 20px lines over `code_background`, a
//! line-number gutter, wrap or a shared horizontal scroll, and the revealed line tinted. Only the
//! rows on screen are laid out; each line is highlighted from the parsed tree when first drawn
//! and kept while it stays near the viewport.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, HighlightStyle, IntoElement, ListAlignment, ListOffset, ListState, ParentElement, Pixels,
    ScrollWheelEvent, SharedString, Styled, StyledText, Window, div, list, prelude::*, px,
};
use gpui_component::ActiveTheme;
use gpui_component::scroll::ScrollableElement;
use t3_core::files::expand_tabs;

use super::preview::{Preview, TextFile};
use crate::theme::{hsla, palette};

const LINE_HEIGHT: f32 = 20.;
const CODE_PADDING: f32 = 8.;
/// Longer lines draw without colors: querying the tree across them costs more than it shows.
const MAX_HIGHLIGHT_BYTES: usize = 4096;
/// Highlighted lines kept regardless of when they were drawn; beyond it, lines not drawn in
/// the last few frames are dropped.
const LINE_CACHE_SOFT_LIMIT: usize = 600;
const KEEP_FRAMES: u64 = 2;

type Runs = Vec<(Range<usize>, HighlightStyle)>;

struct RenderedLine {
    text: SharedString,
    runs: Runs,
    drawn: u64,
}

pub struct CodeView {
    list: ListState,
    file: Option<Rc<TextFile>>,
    lines: HashMap<usize, RenderedLine>,
    frame: u64,
    dark: Option<bool>,
    wrap: bool,
    h_offset: Pixels,
    char_width: Option<Pixels>,
    font: Option<(SharedString, Pixels)>,
    reveal: Option<usize>,
    /// A reveal waiting for the first layout, which knows the viewport height.
    pending_reveal: Option<usize>,
    /// A wrapped reveal at the top of the view, centered once its height is known.
    centering: Option<usize>,
}

impl CodeView {
    pub fn new() -> CodeView {
        CodeView {
            list: ListState::new(0, ListAlignment::Top, px(600.)),
            file: None,
            lines: HashMap::new(),
            frame: 0,
            dark: None,
            wrap: true,
            h_offset: px(0.),
            char_width: None,
            font: None,
            reveal: None,
            pending_reveal: None,
            centering: None,
        }
    }

    /// Shows `file`. A new read of the same file keeps the reader's place.
    pub fn set_file(&mut self, file: Option<Rc<TextFile>>) {
        if let (Some(current), Some(next)) = (&self.file, &file)
            && Rc::ptr_eq(current, next)
        {
            return;
        }
        let same_file = matches!((&self.file, &file), (Some(current), Some(next)) if current.key == next.key);
        let keep = same_file.then(|| self.list.logical_scroll_top());
        self.lines.clear();
        let count = file.as_ref().map_or(0, |file| file.lines.len());
        self.list.reset_with_uniform_height(count, px(LINE_HEIGHT));
        match keep {
            Some(top) if count > 0 => self.list.scroll_to(ListOffset { item_ix: top.item_ix.min(count - 1), offset_in_item: top.offset_in_item }),
            Some(_) => {}
            None => {
                self.h_offset = px(0.);
                self.reveal = None;
                self.pending_reveal = None;
                self.centering = None;
            }
        }
        self.file = file;
    }

    pub fn set_wrap(&mut self, wrap: bool) {
        if wrap != self.wrap {
            self.wrap = wrap;
            self.h_offset = px(0.);
            self.list.remeasure();
        }
    }

    pub fn set_reveal(&mut self, line: Option<usize>) {
        self.reveal = line;
    }

    /// Scrolls `line` to the middle of the view once the list knows its height.
    pub fn reveal(&mut self, line: usize) {
        self.reveal = Some(line);
        self.pending_reveal = Some(line);
    }

    /// Wrapped lines have unknown heights until drawn, so the line first scrolls to the top and
    /// then, a frame later, into the middle by its measured bounds. Returns whether another
    /// frame is needed.
    fn scroll_pending_reveal(&mut self) -> bool {
        let viewport = self.list.viewport_bounds();
        if viewport.size.height <= px(0.) {
            return self.pending_reveal.is_some();
        }
        if let Some(line) = self.centering.take() {
            if let Some(bounds) = self.list.bounds_for_item(line) {
                self.list.scroll_by(bounds.center().y - viewport.center().y);
            }
            return false;
        }
        let Some(line) = self.pending_reveal.take() else { return false };
        if self.wrap {
            self.list.scroll_to(ListOffset { item_ix: line, offset_in_item: px(0.) });
            self.centering = Some(line);
            return true;
        }
        let above = ((f32::from(viewport.size.height) / LINE_HEIGHT - 1.) / 2.).floor().max(0.) as usize;
        self.list.scroll_to(ListOffset { item_ix: line.saturating_sub(above), offset_in_item: px(0.) });
        false
    }

    /// The line as drawn, highlighted on first use. Called only for rows on screen.
    fn line(&mut self, index: usize, cx: &Context<Preview>) -> Option<(SharedString, Runs)> {
        let frame = self.frame;
        if let Some(line) = self.lines.get_mut(&index) {
            line.drawn = frame;
            return Some((line.text.clone(), line.runs.clone()));
        }
        let file = self.file.as_ref()?;
        let range = file.lines.range(index)?;
        let source = &file.contents[range.clone()];
        let runs: Runs = match &file.highlighter {
            Some(highlighter) if source.len() <= MAX_HIGHLIGHT_BYTES && !source.is_empty() => {
                let theme = cx.theme().highlight_theme.clone();
                highlighter
                    .styles(&range, &*theme)
                    .into_iter()
                    .filter(|(_, style)| *style != HighlightStyle::default())
                    .map(|(styled, style)| (styled.start - range.start..styled.end - range.start, style))
                    .collect()
            }
            _ => Vec::new(),
        };
        let (text, runs) = expand_tabs(source, &runs);
        let text = SharedString::from(text);
        self.lines.insert(index, RenderedLine { text: text.clone(), runs: runs.clone(), drawn: frame });
        Some((text, runs))
    }
}

/// Pierre's number column: at least 3ch of digits, 2ch before and 1ch after.
fn gutter_width(digits: usize, char_width: Pixels) -> Pixels {
    char_width * (digits.max(3) + 3) as f32
}

impl Preview {
    pub(super) fn render_code(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let dark = cx.theme().mode.is_dark();
        let code = &mut self.code;
        if code.dark != Some(dark) {
            code.dark = Some(dark);
            code.lines.clear();
        }
        let font = (cx.theme().mono_font_family.clone(), cx.theme().mono_font_size);
        if code.font.as_ref() != Some(&font) {
            code.font = Some(font);
            code.char_width = None;
            code.h_offset = px(0.);
            code.list.remeasure();
        }
        if code.char_width.is_none() {
            let family = cx.theme().mono_font_family.clone();
            let font_id = window.text_system().resolve_font(&gpui::font(family));
            let advance = window.text_system().advance(font_id, cx.theme().mono_font_size, 'm').map(|size| size.width);
            code.char_width = Some(advance.unwrap_or(px(7.8)));
        }
        code.frame += 1;
        if code.lines.len() > LINE_CACHE_SOFT_LIMIT {
            let frame = code.frame;
            code.lines.retain(|_, line| line.drawn + KEEP_FRAMES >= frame);
        }
        if code.scroll_pending_reveal() {
            window.request_animation_frame();
        }
        div()
            .id("file-code")
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(hsla(p.code_background))
            .on_scroll_wheel(cx.listener(Self::scroll_code_horizontally))
            .child(list(self.code.list.clone(), cx.processor(|this, index: usize, _, cx| this.render_line(index, cx))).size_full())
            .vertical_scrollbar(&self.code.list)
            .into_any_element()
    }

    /// Without wrap, long lines scroll sideways together; the gutter stays put.
    fn scroll_code_horizontally(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let code = &mut self.code;
        if code.wrap {
            return;
        }
        let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
        let dx = if event.modifiers.shift && delta.x == px(0.) { delta.y } else { delta.x };
        if !event.modifiers.shift && dx.abs() <= delta.y.abs() {
            return;
        }
        let (Some(file), Some(char_width)) = (code.file.as_ref(), code.char_width) else { return };
        let visible = code.list.viewport_bounds().size.width - gutter_width(file.lines.digits(), char_width) - px(CODE_PADDING * 2.);
        let max = (char_width * file.lines.max_columns as f32 - visible).max(px(0.));
        let next = (code.h_offset - dx).clamp(px(0.), max);
        if next != code.h_offset {
            code.h_offset = next;
            cx.notify();
        }
    }

    fn render_line(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some((text, runs)) = self.code.line(index, cx) else { return div().into_any_element() };
        let p = palette(cx);
        let code = &self.code;
        let char_width = code.char_width.unwrap_or(px(7.8));
        let digits = code.file.as_ref().map_or(3, |file| file.lines.digits());
        let revealed = code.reveal == Some(index);
        let family = cx.theme().mono_font_family.clone();
        let styled = StyledText::new(text).with_highlights(runs);
        div()
            .flex()
            .w_full()
            .when(!code.wrap, |this| this.h(px(crate::theme::code_line_height(cx))))
            .when(code.wrap, |this| this.min_h(px(crate::theme::code_line_height(cx))))
            .bg(if revealed { hsla(p.accent) } else { hsla(p.code_background) })
            .font_family(family)
            .text_size(cx.theme().mono_font_size)
            .line_height(px(crate::theme::code_line_height(cx)))
            .child(
                div()
                    .flex_none()
                    .w(gutter_width(digits, char_width))
                    .pl(char_width * 2.)
                    .pr(char_width)
                    .text_right()
                    .text_color(if revealed { hsla(p.foreground) } else { hsla(p.code_foreground).opacity(0.65) })
                    .child(SharedString::from((index + 1).to_string())),
            )
            .child({
                let column = div().flex_1().min_w_0().px(px(CODE_PADDING)).text_color(hsla(p.code_foreground));
                if code.wrap {
                    column.child(styled)
                } else {
                    column.overflow_hidden().whitespace_nowrap().child(div().ml(-code.h_offset).child(styled))
                }
            })
            .into_any_element()
    }
}
