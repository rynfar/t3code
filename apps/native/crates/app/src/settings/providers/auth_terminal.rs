//! The VT surface for the PTY carried by provider.auth.subscribe/respond.

use std::{cell::RefCell, ops::Range, rc::Rc};

use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, Entity, EntityInputHandler, FocusHandle,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, Point,
    Render, SharedString, StatefulInteractiveElement, Styled, Task, UTF16Selection, Window, div,
    point, px, size,
};
use serde_json::{Value, json};
use t3_core::settings::providers::auth::{self, AuthState, Interaction};
use t3_core::terminal::{
    Grid, KeyInput,
    emulator::{Emulator, TermMode},
    encode_key, encode_paste,
};
use tokio::sync::mpsc;

use super::SettingsView;
use crate::{
    i18n::t,
    terminal::{
        Clear, Copy, Paste, SelectAll, colors,
        element::{RowCache, TerminalElement, TerminalSurface},
        pane::Geometry,
        session::{Frame, Front},
    },
    theme::{hsla, palette, rem},
};

const DATA_LIMIT: usize = 4_096;
const PROVIDERS_MANAGE: &str = t3_core::usage::PROVIDERS_MANAGE_SCOPE;

#[derive(Debug, PartialEq, Eq)]
enum TerminalInput {
    Data(String),
    Resize(Grid),
}

#[derive(Clone, PartialEq, Eq)]
struct Owner {
    environment: String,
    connection: Option<u64>,
    fleet_owner: Option<t3_core::sidebar::grouping::EnvironmentOwner>,
    instance: String,
    flow: String,
    interaction: String,
}

impl Owner {
    fn capture(view: &SettingsView, instance: &str, state: &AuthState, cx: &App) -> Option<Self> {
        let Interaction::Terminal { id, .. } = state.interaction.as_ref()? else {
            return None;
        };
        let app = view.state.read(cx);
        let fleet_owner = view.scoped_primary_owner(cx);
        let config = if let Some(owner) = fleet_owner.as_ref() {
            app.scoped_environment(&owner.saved_id)?
                .config
                .config
                .as_ref()?
        } else {
            app.server_config.config.as_ref()?
        };
        Some(Self {
            environment: config.environment.environment_id.clone(),
            connection: app.connection_id(),
            fleet_owner,
            instance: instance.to_owned(),
            flow: state.flow_id.clone()?,
            interaction: id.clone(),
        })
    }

    fn current(&self, view: &SettingsView, cx: &App) -> bool {
        let current = view.providers.auth.as_ref().and_then(|session| {
            Self::capture(view, &session.instance_id, session.state.as_ref()?, cx)
        });
        current.as_ref() == Some(self)
    }

    fn payload(&self, data: &str, size: Option<Grid>) -> Value {
        let mut response = json!({ "type": "terminal", "data": data });
        if let Some(size) = size {
            response["size"] = json!({ "cols": size.cols, "rows": size.rows });
        }
        json!({
            "instanceId": self.instance,
            "flowId": self.flow,
            "interactionId": self.interaction,
            "response": response,
        })
    }
}

pub(super) fn output_offset(value: &Value) -> Option<usize> {
    value["interaction"]["outputOffset"]
        .as_u64()?
        .try_into()
        .ok()
}

pub(super) struct ActiveTerminal {
    owner: Owner,
    view: Entity<AuthTerminal>,
    _pump: Task<()>,
}

impl SettingsView {
    pub(super) fn render_auth_terminal(
        &mut self,
        instance: &str,
        state: Option<&AuthState>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<AuthTerminal>> {
        let interaction = state.and_then(|state| {
            let Interaction::Terminal { output, .. } = state.interaction.as_ref()? else {
                return None;
            };
            Some((Owner::capture(self, instance, state, cx)?, output))
        });
        let Some((owner, output)) = interaction else {
            self.providers.auth_terminal = None;
            return None;
        };
        if self
            .providers
            .auth_terminal
            .as_ref()
            .is_none_or(|active| active.owner != owner)
        {
            let (input, mut inputs) = mpsc::unbounded_channel();
            let view = cx.new(|cx| AuthTerminal::new(input, cx));
            let target = owner.clone();
            let pump = cx.spawn(async move |this, cx| {
                while let Some(input) = inputs.recv().await {
                    let (data, size) = match &input {
                        TerminalInput::Data(data) => (data.as_str(), None),
                        TerminalInput::Resize(grid) => ("", Some(*grid)),
                    };
                    for part in response_chunks(data).chain(size.map(|_| "")) {
                        let Ok(Some(request)) = this.update(cx, |this, cx| {
                            (target.current(this, cx) && this.can_use_auth_terminal(&target, cx))
                                .then(|| {
                                    this.scoped_rpc(
                                        PROVIDERS_MANAGE,
                                        auth::AUTH_RESPOND,
                                        target.payload(part, size),
                                        cx,
                                    )
                                })
                        }) else {
                            return;
                        };
                        if let Err(error) = request.await {
                            let _ = this.update(cx, |this, cx| {
                                if target.current(this, cx) {
                                    let fallback = t(cx, "Provider sign-in failed. Try again.");
                                    if let Some(session) = this.providers.auth.as_mut() {
                                        session.error = Some(super::failure_text(&error, fallback));
                                    }
                                    cx.notify();
                                }
                            });
                            break;
                        }
                    }
                }
            });
            self.providers.auth_terminal = Some(ActiveTerminal {
                owner,
                view,
                _pump: pump,
            });
        }
        let active = self.providers.auth_terminal.as_ref()?;
        let offset = self
            .providers
            .auth
            .as_ref()
            .and_then(|session| session.output_offset);
        let can_type = self.can_use_auth_terminal(&active.owner, cx);
        active.view.update(cx, |terminal, cx| {
            terminal.can_type = can_type;
            terminal.write(output, offset, cx);
        });
        Some(active.view.clone())
    }

    fn can_use_auth_terminal(&self, target: &Owner, cx: &App) -> bool {
        let app = self.state.read(cx);
        let config = if let Some(owner) = target.fleet_owner.as_ref() {
            app.scoped_environment(&owner.saved_id)
                .and_then(|entry| entry.config.config.as_ref())
        } else {
            app.server_config.config.as_ref()
        };
        !super::fixture_active(cx)
            && self.providers.auth_owner == target.fleet_owner
            && self
                .scoped_session(cx)
                .is_none_or(|session| session.grants(PROVIDERS_MANAGE))
            && config.is_some_and(|config| {
                config.provider(&target.instance).is_some_and(|provider| {
                    t3_core::managed_policy::driver_allowed(
                        config.managed_policy.as_ref(),
                        &provider.driver,
                    )
                })
            })
    }
}

/// The contract measures JavaScript string length in UTF-16 units.
fn response_chunks(mut data: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if data.is_empty() {
            return None;
        }
        let mut units = 0;
        let end = data
            .char_indices()
            .find_map(|(index, ch)| {
                units += ch.len_utf16();
                (units > DATA_LIMIT).then_some(index)
            })
            .unwrap_or(data.len());
        let (part, rest) = data.split_at(end);
        data = rest;
        Some(part)
    })
}

fn offset_delta(output: &str, previous: usize, offset: usize) -> Option<&str> {
    let delta = offset.checked_sub(previous)?;
    let mut remaining = delta;
    let mut start = output.len();
    for (index, ch) in output.char_indices().rev() {
        if remaining == 0 {
            break;
        }
        remaining = remaining.checked_sub(ch.len_utf16())?;
        start = index;
    }
    (remaining == 0).then_some(&output[start..])
}

pub(super) struct AuthTerminal {
    input: mpsc::UnboundedSender<TerminalInput>,
    can_type: bool,
    emulator: Emulator,
    output: String,
    offset: usize,
    fitted: bool,
    sent_grid: Option<Grid>,
    focus: FocusHandle,
    front: Front,
    frame: Rc<RefCell<Frame>>,
    rows: Rc<RefCell<RowCache>>,
    geometry: Option<Geometry>,
    marked: Option<String>,
}

impl AuthTerminal {
    fn new(input: mpsc::UnboundedSender<TerminalInput>, cx: &mut Context<Self>) -> Self {
        Self {
            input,
            can_type: false,
            emulator: Emulator::new(Grid::new(80, 14), colors::theme_colors(cx)),
            output: String::new(),
            offset: 0,
            fitted: false,
            sent_grid: None,
            focus: cx.focus_handle().tab_stop(true),
            front: Front::default(),
            frame: Rc::default(),
            rows: Rc::default(),
            geometry: None,
            marked: None,
        }
    }

    fn send(&self, data: String) {
        if self.can_type && !data.is_empty() {
            let _ = self.input.send(TerminalInput::Data(data));
        }
    }

    fn write(&mut self, output: &str, offset: Option<usize>, cx: &mut Context<Self>) {
        let next_offset = offset.unwrap_or_else(|| output.encode_utf16().count());
        if next_offset == self.offset && output == self.output {
            return;
        }
        if self.fitted {
            let delta = if offset.is_some() {
                offset_delta(output, self.offset, next_offset).filter(|part| !part.is_empty())
            } else {
                output.strip_prefix(&self.output)
            };
            if let Some(delta) = delta {
                let reply = self.emulator.feed(delta.as_bytes());
                self.send(reply);
            } else {
                self.emulator = self.emulator.blank();
                self.emulator.replay(output);
            }
        }
        self.output.clear();
        self.output.push_str(output);
        self.offset = next_offset;
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let key = KeyInput {
            key: &stroke.key,
            key_char: stroke.key_char.as_deref(),
            control: stroke.modifiers.control,
            alt: stroke.modifiers.alt,
            shift: stroke.modifiers.shift,
            platform: stroke.modifiers.platform,
        };
        if self.can_type
            && let Some(bytes) =
                encode_key(&key, self.emulator.mode().contains(TermMode::APP_CURSOR))
        {
            self.send(String::from_utf8_lossy(&bytes).into_owned());
            cx.stop_propagation();
        }
    }
}

impl TerminalSurface for AuthTerminal {
    fn fit(&mut self, grid: Grid, cell: (f32, f32), cx: &mut Context<Self>) {
        // provider.auth.respond has smaller grid limits than terminal.resize.
        let grid = Grid::new(grid.cols.min(500), grid.rows.min(200));
        self.emulator.resize(grid);
        self.emulator.set_theme(colors::theme_colors(cx));
        self.emulator.set_cell_size(cell.0, cell.1);
        if self.can_type
            && self.sent_grid != Some(grid)
            && self.input.send(TerminalInput::Resize(grid)).is_ok()
        {
            self.sent_grid = Some(grid);
        }
        if !self.fitted {
            self.fitted = true;
            let reply = self.emulator.feed(self.output.as_bytes());
            self.send(reply);
        }
        self.emulator
            .snapshot(&mut self.frame.borrow_mut().snapshot);
    }

    fn set_geometry(&mut self, geometry: Geometry) {
        self.geometry = Some(geometry);
    }
}

impl Render for AuthTerminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .id("provider-auth-terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .w_full()
            .h(rem(256.))
            .rounded(rem(6.))
            .border_1()
            .border_color(hsla(p.border))
            .bg(hsla(p.terminal_background))
            .overflow_hidden()
            .cursor_text()
            .tooltip(|window, cx| {
                gpui_component::tooltip::Tooltip::new(t(cx, "Provider sign-in terminal"))
                    .build(window, cx)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus.focus(window, cx);
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|this, _: &Paste, _, cx| {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let bracketed = this.emulator.mode().contains(TermMode::BRACKETED_PASTE);
                    this.send(encode_paste(&text, bracketed));
                }
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| {
                if let Some(text) = this.emulator.selection_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.emulator.select_all();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Clear, _, _| this.send("\x0c".to_owned())))
            .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                let lines = f32::from(event.delta.pixel_delta(px(20.)).y) / 20.;
                this.emulator.scroll(lines.round() as i32);
                cx.notify();
                cx.stop_propagation();
            }))
            .child(TerminalElement::new(
                cx.entity(),
                self.front.clone(),
                self.frame.clone(),
                self.rows.clone(),
                self.focus.clone(),
                self.focus.is_focused(window),
                self.marked.clone().map(SharedString::from),
            ))
    }
}

impl EntityInputHandler for AuthTerminal {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let units: Vec<_> = self.marked.as_ref()?.encode_utf16().collect();
        let range = range.start.min(units.len())..range.end.min(units.len());
        *adjusted = Some(range.clone());
        Some(String::from_utf16_lossy(&units[range]))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .marked
            .as_ref()
            .map_or(0, |text| text.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        self.send(text.to_owned());
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (self.can_type && !text.is_empty()).then(|| text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let geometry = self.geometry?;
        let cursor = self.frame.borrow().snapshot.cursor?;
        Some(Bounds::new(
            geometry.origin
                + point(
                    px(cursor.col as f32 * geometry.cell_width),
                    px(cursor.row as f32 * geometry.line_height),
                ),
            size(px(geometry.cell_width), px(geometry.line_height)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_terminal(
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<AuthTerminal>, mpsc::UnboundedReceiver<TerminalInput>) {
        let (input, inputs) = mpsc::unbounded_channel();
        let terminal = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            cx.new(|cx| AuthTerminal::new(input, cx))
        });
        (terminal, inputs)
    }

    #[gpui::test]
    fn fit_sends_resize_only_when_grid_changes(cx: &mut gpui::TestAppContext) {
        let (terminal, mut inputs) = test_terminal(cx);
        terminal.update(cx, |terminal, cx| {
            terminal.can_type = true;
            terminal.fit(Grid::new(80, 14), (8., 18.), cx);
            assert_eq!(inputs.try_recv().unwrap(), TerminalInput::Resize(Grid::new(80, 14)));
            terminal.fit(Grid::new(80, 14), (9., 18.), cx);
            assert!(inputs.try_recv().is_err());
            terminal.fit(Grid::new(600, 250), (9., 18.), cx);
            assert_eq!(inputs.try_recv().unwrap(), TerminalInput::Resize(Grid::new(500, 200)));
            terminal.fit(Grid::new(610, 250), (9., 18.), cx);
            assert!(inputs.try_recv().is_err());
        });
    }

    #[gpui::test]
    fn recovered_output_does_not_resend_terminal_replies(cx: &mut gpui::TestAppContext) {
        let (terminal, mut inputs) = test_terminal(cx);
        terminal.update(cx, |terminal, cx| {
            terminal.fit(Grid::new(80, 14), (8., 18.), cx);
            terminal.can_type = true;
            terminal.write("one\x1b[6n", None, cx);
            assert_eq!(inputs.try_recv().unwrap(), TerminalInput::Data("\x1b[1;4R".into()));

            terminal.write("reset\x1b[6n", None, cx);
            assert!(inputs.try_recv().is_err());

            terminal.write("reset\x1b[6n!\x1b[6n", None, cx);
            assert_eq!(inputs.try_recv().unwrap(), TerminalInput::Data("\x1b[1;7R".into()));
        });
    }

    #[test]
    fn rolled_output_uses_utf16_offsets_and_recovers_from_gaps() {
        assert_eq!(offset_delta("old🦀a", 25, 28), Some("🦀a"));
        assert_eq!(offset_delta("🦀", 25, 26), None);
        assert_eq!(offset_delta("tail", 25, 30), None);
        assert_eq!(offset_delta("reset", 25, 5), None);
    }

    #[test]
    fn paste_chunks_respect_the_wire_limit_without_splitting_characters() {
        let data = format!("{}🦀\r", "a".repeat(DATA_LIMIT - 1));
        let parts: Vec<_> = response_chunks(&data).collect();
        assert_eq!(parts, [&data[..DATA_LIMIT - 1], "🦀\r"]);
        assert_eq!(parts.concat(), data);
    }
}
