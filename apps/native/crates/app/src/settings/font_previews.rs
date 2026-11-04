//! Embedded local previews from `settings/SettingsFontPreviews.tsx`.

use gpui::Focusable;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, IntoElement, ParentElement, Styled, StyledText,
    Subscription, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme, Rope, h_flex,
    highlighter::SyntaxHighlighter,
    input::{Copy, Cut, InputEvent, Paste, Textarea, TextareaState},
    v_flex,
};
use t3_core::{
    client_settings::typography::FontRole,
    diff::{DiffLine, LineKind, ParsedPatch, parse_patch},
};

use super::super::SettingsView;
use crate::{
    diff::lines::{code_row, gutter_width, line_colors},
    theme::{self, hsla, palette},
};

#[path = "font_preview_terminal.rs"]
mod terminal;

const PROMPT: &str = "Use $frontend-design to fix the flaky test in [surface.test.ts](apps/web/src/terminal/ghostty/surface.test.ts) and align the header with [SettingsPanels.tsx](apps/web/src/components/settings/SettingsPanels.tsx) before shipping.";
const PATCH: &str = "diff --git a/src/formatUser.ts b/src/formatUser.ts\n--- a/src/formatUser.ts\n+++ b/src/formatUser.ts\n@@ -1,3 +1,3 @@\n export function formatUser(user: User) {\n-  return user.name.toUpperCase();\n+  return `${user.name} <${user.email}>`; // 0O 1lI\n }\n";

pub(super) struct FontPreviews {
    prompt: Entity<TextareaState>,
    terminal: Entity<terminal::TerminalPreview>,
    lines: Vec<(DiffLine, SyntaxHighlighter)>,
    _prompt_change: Subscription,
}

impl FontPreviews {
    pub fn new(window: &mut Window, cx: &mut Context<SettingsView>) -> Self {
        let prompt = cx.new(|cx| {
            let mut input = TextareaState::new(window, cx)
                .auto_grow(2, 7)
                .placeholder("Ask anything. @ files, $ skills, / commands")
                .submit_on_enter(false);
            input.set_value(PROMPT, window, cx);
            input
        });
        #[cfg(feature = "snapshot")]
        crate::snapshot::register_focus(
            "prompt-font-preview",
            prompt.read(cx).focus_handle(cx),
            cx,
        );
        let repaint = cx.subscribe(&prompt, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let terminal = cx.new(|cx| terminal::TerminalPreview::new(window, cx));
        let ParsedPatch::Files(files) = parse_patch(PATCH) else {
            unreachable!("preview patch")
        };
        let lines = files[0].hunks[0]
            .lines
            .iter()
            .map(|line| {
                let mut syntax = SyntaxHighlighter::new("typescript");
                syntax.update(None, &Rope::from_str(&line.text), None);
                (line.clone(), syntax)
            })
            .collect();
        Self {
            prompt,
            terminal,
            lines,
            _prompt_change: repaint,
        }
    }

    pub fn focused(&self, window: &Window, cx: &App) -> bool {
        self.prompt.read(cx).focus_handle(cx).is_focused(window)
            || self.terminal.read(cx).focus_handle(cx).is_focused(window)
    }

    pub fn render(&self, role: FontRole, cx: &App) -> AnyElement {
        let p = palette(cx);
        match role {
            FontRole::Interface | FontRole::Prompt => {
                let size = theme::font_size(FontRole::Prompt, cx);
                // The native composer edits skills and serialized file links as plain text too.
                div()
                    .mt(theme::rem(4.))
                    .mb(theme::rem(8.))
                    .rounded(theme::rem(8.))
                    .border_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.background))
                    .px(theme::rem(12.))
                    .py(theme::rem(8.))
                    .capture_action(|_: &Copy, _, cx| cx.stop_propagation())
                    .capture_action(|_: &Cut, _, cx| cx.stop_propagation())
                    .capture_action(|_: &Paste, _, cx| cx.stop_propagation())
                    .child(
                        Textarea::new(&self.prompt)
                            .appearance(false)
                            .aria_label("Prompt font preview")
                            .context_menu(|menu, _, _| menu)
                            .font_family(theme::font_family(FontRole::Prompt, cx))
                            .text_size(px(size))
                            .line_height(px(size * 1.625)),
                    )
                    .into_any_element()
            }
            FontRole::Code => self.code(cx),
            FontRole::Terminal => div()
                .mt(theme::rem(4.))
                .mb(theme::rem(8.))
                .child(self.terminal.clone())
                .into_any_element(),
        }
    }

    fn code(&self, cx: &App) -> AnyElement {
        let p = palette(cx);
        let wrap = theme::preferences(cx).typography.word_wrap;
        let char_width = cx
            .text_system()
            .advance(
                cx.text_system()
                    .resolve_font(&gpui::font(theme::font_family(FontRole::Code, cx))),
                px(theme::font_size(FontRole::Code, cx)),
                'm',
            )
            .map_or(px(7.8), |size| size.width);
        let content = v_flex()
            .mt(theme::rem(4.))
            .mb(theme::rem(8.))
            .min_w_0()
            .bg(hsla(p.code_background))
            .text_color(hsla(p.code_foreground))
            .child(
                h_flex()
                    .h(px(32.))
                    .px(px(12.))
                    .text_size(px(12.))
                    .child("src/formatUser.ts")
                    .child(div().flex_1())
                    .child(crate::diff::lines::stat_label(1, 1, cx)),
            )
            .children(self.lines.iter().map(|(line, syntax)| {
                let mut colors = line_colors(line.kind, p);
                if line.kind == LineKind::Context {
                    colors.row = hsla(p.code_background);
                    colors.gutter = hsla(p.code_background);
                }
                let number = if line.kind == LineKind::Deletion {
                    line.old_line
                } else {
                    line.new_line
                };
                let gutter = div()
                    .relative()
                    .flex_none()
                    .w(gutter_width(1, char_width))
                    .pl(char_width * 2.)
                    .pr(char_width)
                    .bg(colors.gutter)
                    .text_color(colors.number)
                    .text_right()
                    .children(colors.bar.map(|bar| {
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(px(4.))
                            .bg(bar)
                    }))
                    .children(number.map(|n| n.to_string()));
                let text = StyledText::new(line.text.clone()).with_highlights(
                    syntax.styles(&(0..line.text.len()), &*cx.theme().highlight_theme),
                );
                code_row(colors.row, wrap, cx).child(gutter).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(px(8.))
                        .when(!wrap, |this| this.whitespace_nowrap())
                        .child(text),
                )
            }));
        let width = char_width
            * self
                .lines
                .iter()
                .map(|(line, _)| line.text.chars().count())
                .max()
                .unwrap_or(0) as f32
            + gutter_width(1, char_width)
            + px(16.);
        div()
            .id("code-font-preview")
            .w_full()
            .min_w_0()
            .when(!wrap, |this| this.overflow_x_scroll())
            .child(content.when(!wrap, |this| this.min_w(width)))
            .into_any_element()
    }
}

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn typing_paths_in_previews_keeps_focus_out_of_settings_search(cx: &mut TestAppContext) {
        let home = std::env::temp_dir().join(format!("t3-font-previews-{}", uuid::Uuid::new_v4()));
        t3_client::store::use_home(home);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            let state = cx.new(|cx| crate::app_state::AppState::new(runtime.handle().clone(), cx));
            crate::settings::init(state.clone(), cx);
            state
        });
        let window = cx.add_window(|_, _| gpui::Empty);
        let view = window
            .update(cx, |_, window, cx| {
                cx.new(|cx| {
                    SettingsView::new(
                        state,
                        t3_core::settings::search::Section::Appearance,
                        None,
                        window,
                        cx,
                    )
                })
            })
            .unwrap();
        window
            .update(cx, |_, window, cx| {
                view.update(cx, |view, cx| {
                    let slash = gpui::KeyDownEvent {
                        keystroke: gpui::Keystroke::parse("/").unwrap(),
                        is_held: false,
                        prefer_character_input: false,
                    };
                    let prompt = view.typography.previews.prompt.read(cx).focus_handle(cx);
                    let terminal = view.typography.previews.terminal.read(cx).focus_handle(cx);
                    for focus in [prompt, terminal] {
                        focus.focus(window, cx);
                        view.on_key_down(&slash, window, cx);
                        assert!(
                            focus.is_focused(window),
                            "typing a path must keep preview focus"
                        );
                        assert!(!view.search.read(cx).focus_handle(cx).is_focused(window));
                    }
                })
            })
            .unwrap();
    }
}
