//! An expanded command row's output: `CommandOutputPanel.tsx`. `t3_client::command_output`
//! subscribes and folds frames on the network runtime; a slot holds the newest result, pins the
//! scroller to the tail while the reader stays at the bottom, and draws the web's loading,
//! empty, error and truncation states. The output is selectable text, as the browser makes it on
//! the web. Nothing here ticks: a slot redraws only when a frame lands.

use std::cell::RefCell;

use gpui::{
    AnyElement, App, FontStyle, FontWeight, HighlightStyle, InteractiveElement, IntoElement, ParentElement, ScrollHandle, SharedString, StatefulInteractiveElement,
    StrikethroughStyle, Styled, Task, UnderlineStyle, div, prelude::FluentBuilder, px,
};
use gpui_component::{ActiveTheme, v_flex};
use t3_client::command_output::CommandOutputStream;
use t3_core::command_output::{CommandOutput, CommandOutputStatus, LOADING, NO_OUTPUT, Panel, TRUNCATED};
use t3_core::terminal_output::{DARK_PALETTE, LIGHT_PALETTE, Rgb, SpanStyle, terminal_output_display};

use super::parts::{mono_block, mono_frame};
use crate::selectable_text::{Highlights, SelectableText};
use crate::theme::{hsla, palette, rem};

/// `FOLLOW_SLACK_PX`: a reader this close to the bottom keeps following new output.
const FOLLOW_SLACK: f32 = 8.;
/// `mono_frame`'s line: code size on a 1.625 line.
const LINE_HEIGHT: f32 = crate::theme::metrics::DEFAULT_CODE_FONT_SIZE * 1.625;

/// One open command row's subscription. Dropping it interrupts the stream.
pub struct CommandOutputSlot {
    /// The thread and item it streams, and the connection it streams on.
    pub target: (String, String, Option<u64>),
    status: CommandOutputStatus,
    scroll: ScrollHandle,
    /// The text without escapes and its styled ranges, for the appearance it was built in.
    display: RefCell<Option<(bool, Display)>>,
    _stream: CommandOutputStream,
    _pump: Task<()>,
}

#[derive(Clone)]
struct Display {
    text: SharedString,
    highlights: Highlights,
}

impl CommandOutputSlot {
    pub fn new(target: (String, String, Option<u64>), stream: CommandOutputStream, pump: Task<()>) -> Self {
        CommandOutputSlot {
            target,
            status: CommandOutputStatus::default(),
            scroll: ScrollHandle::new(),
            display: RefCell::new(None),
            _stream: stream,
            _pump: pump,
        }
    }

    /// Takes the newest output; returns whether the row's height may have changed.
    pub fn update(&mut self, status: CommandOutputStatus) -> bool {
        // Measured against the last layout, before the new output lands: only a reader already
        // at the bottom is pinned to it, and output that already filled the cap keeps it full.
        let following = f32::from(self.scroll.max_offset().y + self.scroll.offset().y) <= FOLLOW_SLACK;
        let filled = self.scroll.max_offset().y > px(0.);
        let visible_lines = f32::from(self.scroll.bounds().size.height) / LINE_HEIGHT;
        let resized = !(filled && status.keeps_capped_height(&self.status, visible_lines));
        let changed = status.output.as_ref().map(|output| &output.text) != self.status.output.as_ref().map(|output| &output.text);
        self.status = status;
        if changed {
            self.display.take();
        }
        if following {
            self.scroll.scroll_to_bottom();
        }
        resized
    }

    fn display(&self, output: &CommandOutput, dark: bool) -> Display {
        if let Some((built_dark, display)) = self.display.borrow().as_ref()
            && *built_dark == dark
        {
            return display.clone();
        }
        let display = build_display(&output.text, if dark { &DARK_PALETTE } else { &LIGHT_PALETTE });
        *self.display.borrow_mut() = Some((dark, display.clone()));
        display
    }
}

fn color(rgb: Rgb) -> gpui::Hsla {
    hsla(u32::from(rgb.r) << 24 | u32::from(rgb.g) << 16 | u32::from(rgb.b) << 8 | 0xff)
}

fn highlight(style: &SpanStyle) -> HighlightStyle {
    let dim = if style.dim { 0.65 } else { 1. };
    HighlightStyle {
        color: style.color.map(color),
        // CSS `opacity` dims the fill too; `fade_out` reaches only the text.
        background_color: style.background.map(|rgb| color(rgb).opacity(dim)),
        font_weight: style.bold.then_some(FontWeight::SEMIBOLD),
        font_style: style.italic.then_some(FontStyle::Italic),
        underline: style.underline.then_some(UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
        strikethrough: style.strike.then_some(StrikethroughStyle { thickness: px(1.), color: None }),
        fade_out: style.dim.then_some(0.35),
    }
}

fn build_display(text: &str, palette: &[Rgb; 16]) -> Display {
    let display = terminal_output_display(text, palette);
    Display { text: display.text.into(), highlights: display.styles.iter().map(|(range, style)| (range.clone(), highlight(style))).collect() }
}

fn note(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    mono_block(text, cx).italic().text_color(hsla(palette(cx).muted_foreground)).into_any_element()
}

/// The output slot of an open command row; `None` while a running command has printed nothing.
pub fn panel(entry_id: &str, slot: Option<&CommandOutputSlot>, cx: &App) -> Option<AnyElement> {
    let Some(slot) = slot else { return Some(note(LOADING, cx)) };
    let output = match slot.status.panel() {
        Panel::Loading => return Some(note(LOADING, cx)),
        Panel::Waiting => return None,
        Panel::Empty => return Some(note(NO_OUTPUT, cx)),
        Panel::Failed(error) => {
            let text = format!("Couldn't load output: {error}");
            return Some(mono_block(text, cx).text_color(hsla(palette(cx).destructive)).into_any_element());
        }
        Panel::Output(output) => output,
    };
    let display = slot.display(output, cx.theme().is_dark());
    let scroller = div()
        .id(SharedString::from(format!("command-output:{entry_id}")))
        .w_full()
        .max_h(rem(320.))
        .overflow_y_scroll()
        .track_scroll(&slot.scroll)
        .child(
            mono_frame(cx)
                .text_color(hsla(palette(cx).muted_foreground))
                .child(
                    SelectableText::new(SharedString::from(format!("command-output-text:{entry_id}")), display.text)
                        .highlights(display.highlights)
                        .origin(output.origin)
                        .autoscroll(&slot.scroll),
                ),
        );
    Some(v_flex().w_full().min_w_0().when(output.truncated, |this| this.child(note(TRUNCATED, cx))).child(scroller).into_any_element())
}

/// `ShellCommandBlock.tsx` and `HighlightedTokens.tsx`, shared by streamed command rows and
/// tool inspectors. The keyed state lives while drawn; the bounded cache survives scrolling.
#[derive(gpui::IntoElement)]
pub struct ShellCommandBlock {
    id: SharedString,
    command: SharedString,
}

impl ShellCommandBlock {
    pub fn new(id: &str, command: SharedString) -> Self {
        Self {
            id: SharedString::from(format!("shell-command:{id}")),
            command,
        }
    }
}

struct CommandSyntax {
    command: SharedString,
    theme: std::sync::Arc<gpui_component::highlighter::HighlightTheme>,
    highlights: Option<Highlights>,
    _task: Task<()>,
}

impl CommandSyntax {
    fn new(
        command: SharedString,
        theme: std::sync::Arc<gpui_component::highlighter::HighlightTheme>,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let source = command.clone();
        let colors = theme.clone();
        let task = cx.spawn(async move |this, cx| {
            let highlights = cx
                .background_executor()
                .spawn(async move { cached_command(&source, colors) })
                .await;
            let _ = this.update(cx, |state, cx| {
                state.highlights = Some(highlights);
                cx.notify();
            });
        });
        Self {
            command,
            theme,
            highlights: None,
            _task: task,
        }
    }
}

impl gpui::RenderOnce for ShellCommandBlock {
    fn render(self, window: &mut gpui::Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().highlight_theme.clone();
        let command = self.command;
        let state = window.use_keyed_state(gpui::ElementId::NamedInteger(self.id.clone(), 0), cx, |_, cx| {
            CommandSyntax::new(command.clone(), theme.clone(), cx)
        });
        let highlights = state.update(cx, |state, cx| {
            if state.command != command || !std::sync::Arc::ptr_eq(&state.theme, &theme) {
                *state = CommandSyntax::new(command.clone(), theme, cx);
            }
            state.highlights.clone()
        });
        let mut text = SelectableText::new(self.id, command);
        if let Some(highlights) = highlights {
            text = text.highlights(highlights);
        }
        div()
            .w_full()
            .min_w_0()
            .text_color(hsla(palette(cx).foreground).opacity(0.85))
            .child(text)
    }
}

fn cached_command(
    command: &str,
    theme: std::sync::Arc<gpui_component::highlighter::HighlightTheme>,
) -> Highlights {
    use std::collections::VecDeque;
    use std::sync::{Arc, LazyLock, Mutex};
    type Cached = (
        String,
        Arc<gpui_component::highlighter::HighlightTheme>,
        Highlights,
    );
    static CACHE: LazyLock<Mutex<VecDeque<Cached>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));
    const LIMIT: usize = 64;
    const MAX_BYTES: usize = 64 * 1024;
    if command.len() > MAX_BYTES {
        return Arc::from([]);
    }
    // Only background workers take this lock, so concurrent requests parse a command once.
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(index) = cache
        .iter()
        .position(|(text, colors, _)| text == command && colors.style.syntax == theme.style.syntax)
    {
        let hit = cache.remove(index).expect("cached index");
        let result = hit.2.clone();
        cache.push_back(hit);
        return result;
    }
    let mut budget = 64;
    let styles = command_styles(command, t3_core::shell_command::highlight_language(command), &theme, 0, &mut budget);
    let styles: Highlights = styles.into();
    if cache.len() == LIMIT {
        cache.pop_front();
    }
    cache.push_back((command.to_owned(), theme, styles.clone()));
    styles
}

fn command_styles(
    code: &str,
    language: &str,
    theme: &gpui_component::highlighter::HighlightTheme,
    depth: usize,
    budget: &mut usize,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    use gpui_component::{Rope, highlighter::{LanguageConfig, LanguageRegistry, SyntaxHighlighter}};
    use t3_core::shell_command::grammars;
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        for name in grammars::LANGUAGES {
            let grammar = grammars::grammar(name).expect("command grammar");
            LanguageRegistry::singleton().register(name, &LanguageConfig::new(name, grammar.language, vec![], &grammar.highlights, "", ""));
        }
    });
    if *budget == 0 {
        return Vec::new();
    }
    *budget -= 1;
    let mut highlighter = SyntaxHighlighter::new(language);
    highlighter.update(None, &Rope::from_str(code), None);
    if highlighter.tree().is_none() { return Vec::new(); }
    let mut styles = vec![HighlightStyle::default(); code.len()];
    for (range, style) in highlighter.styles(&(0..code.len()), theme) {
        styles[range].fill(style);
    }
    if language == "bash"
        && depth < 4
        && let Some(tree) = highlighter.tree()
    {
        for script in t3_core::shell_command::embedded_scripts(code, tree) {
            let embedded = command_styles(&script.text, script.language, theme, depth + 1, budget);
            for (range, style) in embedded {
                if let (Some(first), Some(last)) = (
                    script.spans.get(range.start),
                    script.spans.get(range.end - 1),
                ) {
                    styles[first.start..last.end].fill(style);
                }
            }
        }
    }
    let mut runs = Vec::new();
    let mut start = 0;
    while start < styles.len() {
        let style = styles[start];
        let end = styles[start..]
            .iter()
            .position(|next| *next != style)
            .map_or(styles.len(), |length| start + length);
        // Embedded languages also clear the shell string's color on their unstyled tokens.
        runs.push((start..end, style));
        start = end;
    }
    runs
}
