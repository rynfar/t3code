//! The file surfaces of the right panel. Ports `components/files/FilePreviewPanel.tsx` (the
//! breadcrumb subheader and its toggles, the preview beside a resizable explorer, folders that
//! show the explorer instead), `FileExplorerPane.tsx` and the refresh-on-mutation wiring of
//! `useWorkspaceMutationRefresh`. Listing and reading run on the network runtime; parsing and
//! decoding run on the background executor; views paint only what is on screen.

mod actions;
mod code;
mod explorer;
mod explorer_rows;
mod image;
mod preview;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, Context, DragMoveEvent, canvas, EmptyView, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString,
    Styled, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::{h_flex, v_flex};
use serde_json::{Value, json};
use t3_client::store;
use t3_client::supervisor::ConnectionStatus;
use t3_core::diff::{turn_diff_summaries, workspace_mutation_id};
use t3_core::files::{breadcrumbs, is_host_path, is_markdown};
use t3_core::right_panel::SurfaceKind;

pub use actions::FileWorkspace;
pub use explorer::init;

use crate::app_state::{AppEvent, AppState, Phase};
use crate::icons::Icon;
use crate::right_panel::{Button, Variant};
use crate::theme::{hsla, palette};
use explorer::Explorer;
use preview::Preview;

const STORE: &str = "files";
/// `FileExplorerPane`: default 256, between 160 and 560, leaving the file at least 256.
const EXPLORER_WIDTH: f32 = 256.;
const EXPLORER_MIN_WIDTH: f32 = 160.;
const EXPLORER_MAX_WIDTH: f32 = 560.;
const FILE_MIN_WIDTH: f32 = 256.;

/// `FileExplorerPane`'s maximum: what leaves the file its minimum, within 160..560.
fn explorer_max_width(row: f32) -> f32 {
    if row <= 0. { EXPLORER_MAX_WIDTH } else { (row - FILE_MIN_WIDTH).clamp(EXPLORER_MIN_WIDTH, EXPLORER_MAX_WIDTH) }
}

/// The workspace a panel shows, on one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub environment: String,
    pub workspace: FileWorkspace,
}

/// `t3code.fileExplorerOpen`, `t3code.fileExplorerWidth` and `t3code.renderMarkdown`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Prefs {
    explorer_open: bool,
    explorer_width: f32,
    render_markdown: bool,
}

impl Prefs {
    fn load() -> Prefs {
        let value = store::load_ui_state(STORE).unwrap_or(Value::Null);
        Prefs {
            explorer_open: value.get("explorerOpen").and_then(Value::as_bool).unwrap_or(true),
            explorer_width: value
                .get("explorerWidth")
                .and_then(Value::as_f64)
                .map(|width| width as f32)
                .filter(|width| width.is_finite())
                .unwrap_or(EXPLORER_WIDTH),
            render_markdown: value.get("renderMarkdown").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    fn save(&self) {
        store::save_ui_state(
            STORE,
            &json!({ "explorerOpen": self.explorer_open, "explorerWidth": self.explorer_width, "renderMarkdown": self.render_markdown }),
        );
    }
}

/// The active file tab: a path (`None` for the explorer on its own) and its reveal request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    surface_id: String,
    path: Option<String>,
    reveal_line: Option<u64>,
    reveal_request_id: u64,
}

struct ExplorerResize;

pub struct FilesPanel {
    state: Entity<AppState>,
    explorer: Entity<Explorer>,
    preview: Entity<Preview>,
    root: Option<Root>,
    shown: Option<Shown>,
    prefs: Prefs,
    /// The session's word wrap override; the web's `wordWrap` setting defaults to on.
    wrap: bool,
    connected: bool,
    checkpoints: (usize, Option<String>),
    latest_checkpoint: Option<String>,
    mutation: Option<String>,
    /// A workspace mutation arrived since the last refresh; it runs once the files show.
    stale: bool,
    /// The connection came back; failed reads retry once the files show.
    reconnected: bool,
    /// The files body's width at the last paint, which bounds the explorer.
    body_width: Rc<Cell<f32>>,
    _save_width: Option<Task<()>>,
    _subscription: Subscription,
}

impl FilesPanel {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let explorer = cx.new(|cx| Explorer::new(state.clone(), window, cx));
        let preview = cx.new(|cx| Preview::new(state.clone(), cx));
        let subscription = cx.subscribe(&state, |this, _, event: &AppEvent, cx| {
            if matches!(event, AppEvent::Thread | AppEvent::Selection | AppEvent::RightPanel | AppEvent::Shell | AppEvent::Phase) {
                this.sync(cx);
            }
        });
        cx.observe(&preview, |this, preview, cx| {
            // A path that reads as a folder opens in the explorer (`FileBrowserPanel` reveal).
            if preview.read(cx).is_folder()
                && let Some(path) = this.shown.as_ref().and_then(|shown| shown.path.clone())
            {
                this.explorer.update(cx, |explorer, cx| explorer.expand_folder(&path, cx));
            }
            cx.notify();
        })
        .detach();
        let prefs = Prefs::load();
        preview.update(cx, |preview, cx| preview.set_render_markdown(prefs.render_markdown, cx));
        let mut panel = FilesPanel {
            state,
            explorer,
            preview,
            root: None,
            shown: None,
            prefs,
            wrap: true,
            connected: false,
            checkpoints: (0, None),
            latest_checkpoint: None,
            mutation: None,
            stale: false,
            reconnected: false,
            body_width: Rc::new(Cell::new(0.)),
            _save_width: None,
            _subscription: subscription,
        };
        panel.sync(cx);
        panel
    }

    /// Reconciles with app state; cheap when nothing it shows changed.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let root = state.environment().zip(state.file_workspace()).map(|(environment, workspace)| Root { environment: environment.to_owned(), workspace });
        let connected = matches!(state.phase, Phase::Online(ConnectionStatus::Connected));
        let shown = state.selected_thread.as_deref().and_then(|thread| state.panels.panels.active_surface(thread)).and_then(|surface| match &surface.kind {
            SurfaceKind::Files => Some(Shown { surface_id: surface.id.clone(), path: None, reveal_line: None, reveal_request_id: 0 }),
            SurfaceKind::File { relative_path, reveal_line, reveal_request_id } => Some(Shown {
                surface_id: surface.id.clone(),
                path: Some(relative_path.clone()),
                reveal_line: *reveal_line,
                reveal_request_id: *reveal_request_id,
            }),
            SurfaceKind::Diff | SurfaceKind::PullRequests | SurfaceKind::SideChat { .. } => None,
        });
        let projection = state
            .thread
            .projection
            .as_ref()
            .filter(|projection| state.selected_thread.as_deref() == Some(projection.thread.id.as_str()));
        let checkpoints = projection
            .map(|projection| (projection.checkpoints.len(), projection.checkpoints.last().map(|checkpoint| checkpoint.id.clone())))
            .unwrap_or((0, None));
        if checkpoints != self.checkpoints {
            self.latest_checkpoint = projection
                .and_then(|projection| turn_diff_summaries(&projection.checkpoints).into_iter().next())
                .map(|summary| summary.completed_at);
            self.checkpoints = checkpoints;
        }
        let mutation = projection.and_then(|projection| {
            workspace_mutation_id(projection.visible_turn_items.iter().map(|row| row.item.as_ref()), self.latest_checkpoint.as_deref())
        });
        let visible = shown.is_some();
        let root_changed = root != self.root;
        if root_changed {
            self.root = root.clone();
            self.stale = false;
        } else if mutation != self.mutation && self.mutation.is_some() {
            self.stale = true;
        }
        self.mutation = mutation;
        self.reconnected |= connected && !self.connected;
        self.connected = connected;
        // Hidden files do no work: they catch up when they show.
        if visible {
            self.explorer.update(cx, |explorer, cx| explorer.set_root(root.clone(), cx));
            let target = shown.as_ref().zip(root.as_ref()).and_then(|(shown, root)| {
                Some(preview::Target {
                    root: root.clone(),
                    path: shown.path.clone()?,
                    reveal_line: shown.reveal_line,
                    reveal_request_id: shown.reveal_request_id,
                })
            });
            self.preview.update(cx, |preview, cx| preview.set_target(target, cx));
            if std::mem::take(&mut self.stale) {
                self.explorer.update(cx, |explorer, cx| explorer.refresh(cx));
                self.preview.update(cx, |preview, cx| preview.refetch(cx));
            }
            if std::mem::take(&mut self.reconnected) {
                self.explorer.update(cx, |explorer, cx| explorer.reconnected(cx));
                self.preview.update(cx, |preview, cx| preview.reconnected(cx));
            }
            let selected = shown.as_ref().and_then(|shown| shown.path.clone()).filter(|path| !is_host_path(path));
            let reveal = shown.as_ref().map_or(0, |shown| shown.reveal_request_id);
            self.explorer.update(cx, |explorer, cx| explorer.set_selected(selected, reveal, cx));
        }
        if shown != self.shown || root_changed {
            self.shown = shown;
            cx.notify();
        }
    }

    fn set_prefs(&mut self, change: impl FnOnce(&mut Prefs), cx: &mut Context<Self>) {
        change(&mut self.prefs);
        self.prefs.save();
        let render_markdown = self.prefs.render_markdown;
        self.preview.update(cx, |preview, cx| preview.set_render_markdown(render_markdown, cx));
        cx.notify();
    }

    fn resize_explorer(&mut self, event: &DragMoveEvent<ExplorerResize>, _: &mut Window, cx: &mut Context<Self>) {
        let max = explorer_max_width(f32::from(event.bounds.size.width));
        let width = f32::from(event.bounds.right() - event.event.position.x).clamp(EXPLORER_MIN_WIDTH, max);
        if width == self.prefs.explorer_width {
            return;
        }
        self.prefs.explorer_width = width;
        cx.notify();
        let prefs = self.prefs;
        self._save_width = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            prefs.save();
        }));
    }

    fn render_subheader(&self, path: &str, previewing: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let root = self.root.as_ref();
        let project = root.map_or("", |root| root.workspace.project_name.as_str());
        let surface_id = self.shown.as_ref().map(|shown| shown.surface_id.clone()).unwrap_or_default();
        let is_folder = !previewing;
        let crumbs = breadcrumbs(project, path);
        let crumb_row = h_flex().id("file-breadcrumbs").min_w_0().flex_1().overflow_x_scroll().text_xs().children(crumbs.into_iter().enumerate().flat_map(|(index, crumb)| {
            let label: SharedString = crumb.label.clone().into();
            let separator = (index > 0).then(|| Icon::ChevronRight.svg().flex_none().size(px(12.)).mx(px(2.)).text_color(hsla(p.muted_foreground).opacity(0.6)).into_any_element());
            let state = self.state.clone();
            let surface_id = surface_id.clone();
            let folder = crumb.path.clone();
            let current = crumb.is_last;
            let item = div()
                .id(("file-crumb", index))
                .flex_none()
                .px(px(4.))
                .py(px(2.))
                .rounded(px(4.))
                .whitespace_nowrap()
                .when(current, |this| this.font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)))
                .when(!current, |this| {
                    this.text_color(hsla(p.muted_foreground))
                        .cursor_pointer()
                        .hover(|style| style.bg(hsla(p.accent)).text_color(hsla(p.foreground)))
                        .on_click(move |_, _, cx| {
                            state.update(cx, |state, cx| state.navigate_file_folder(&surface_id, &folder, !is_folder, cx));
                        })
                })
                .child(label)
                .into_any_element();
            separator.into_iter().chain([item]).collect::<Vec<_>>()
        }));
        let markdown = previewing && is_markdown(path);
        let rendered = markdown && self.prefs.render_markdown;
        let raw_text = previewing && self.preview.read(cx).shows_text() && !rendered;
        let host = is_host_path(path);
        let entity = cx.entity().downgrade();
        let toggle = move |run: fn(&mut FilesPanel, &mut Context<FilesPanel>)| {
            let entity = entity.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, run);
                }
            }
        };
        h_flex()
            .flex_none()
            .h(px(28.))
            .mb(px(12.))
            .px(px(12.))
            .gap(px(8.))
            .bg(hsla(p.background))
            .child(crumb_row)
            .when(markdown, |this| {
                this.child(
                    Button::new("file-render-markdown")
                        .variant(Variant::Toggle)
                        .icon(if rendered { Icon::CodeXml } else { Icon::Eye })
                        .icon_size(14.)
                        .pressed(rendered)
                        .tooltip(if rendered { "Show markdown source" } else { "Show rendered markdown" })
                        .on_click(toggle(|this, cx| this.set_prefs(|prefs| prefs.render_markdown = !prefs.render_markdown, cx))),
                )
            })
            .when(raw_text, |this| {
                this.child(
                    Button::new("file-wrap")
                        .variant(Variant::Toggle)
                        .icon(Icon::TextWrap)
                        .icon_size(14.)
                        .pressed(self.wrap)
                        .tooltip(if self.wrap { "Disable word wrap" } else { "Enable word wrap" })
                        .on_click(toggle(|this, cx| {
                            this.wrap = !this.wrap;
                            let wrap = this.wrap;
                            this.preview.update(cx, |preview, cx| preview.set_wrap(wrap, cx));
                            cx.notify();
                        })),
                )
            })
            .when(!host && previewing, |this| {
                let open = self.prefs.explorer_open;
                this.child(
                    Button::new("file-explorer-toggle")
                        .variant(Variant::Toggle)
                        .icon(Icon::FolderTree)
                        .icon_size(14.)
                        .pressed(open)
                        .tooltip(if open { "Hide file explorer" } else { "Show file explorer" })
                        .on_click(toggle(|this, cx| this.set_prefs(|prefs| prefs.explorer_open = !prefs.explorer_open, cx))),
                )
            })
    }

    fn render_explorer_pane(&self, resizable: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let width = self.prefs.explorer_width.clamp(EXPLORER_MIN_WIDTH, explorer_max_width(self.body_width.get()));
        let dragging = cx.has_active_drag();
        div()
            .relative()
            .flex()
            .min_h_0()
            .min_w_0()
            .bg(hsla(p.background))
            .when(resizable, |this| this.flex_none().w(px(width)).border_l_1().border_color(hsla(p.border).opacity(0.6)))
            .when(!resizable, |this| this.flex_1())
            .when(resizable, |this| {
                this.child(
                    div()
                        .id("file-explorer-resize")
                        .group("explorer-resize")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(-4.))
                        .w(px(8.))
                        .cursor_col_resize()
                        .on_drag(ExplorerResize, |_, _, _, cx| cx.new(|_| EmptyView))
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(3.5))
                                .w(px(1.))
                                .when(dragging, |this| this.bg(hsla(p.primary).opacity(0.6)))
                                .when(!dragging, |this| this.group_hover("explorer-resize", |style| style.bg(hsla(p.border)))),
                        ),
                )
            })
            .child(self.explorer.clone())
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        if self.root.is_none() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child("Files are only available when a project is open.")
                .into_any_element();
        }
        let path = self.shown.as_ref().and_then(|shown| shown.path.clone());
        let preview = self.preview.read(cx);
        let previewing = path.is_some() && !preview.is_folder();
        let notice = previewing.then(|| preview.truncation_notice()).flatten();
        let host = path.as_deref().is_some_and(is_host_path);
        let show_explorer = !host && (self.prefs.explorer_open || !previewing);
        v_flex()
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .children(path.as_deref().map(|path| self.render_subheader(path, previewing, cx)))
            .children(notice.map(|notice| {
                div()
                    .flex_none()
                    .border_b_1()
                    .border_color(hsla(p.warning).opacity(0.2))
                    .bg(hsla(p.warning_surface))
                    .px(px(12.))
                    .py(px(6.))
                    .text_size(px(11.))
                    .text_color(hsla(p.warning_foreground))
                    .child(notice)
            }))
            .child(
                div()
                    .id("files-body")
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .on_drag_move(cx.listener(Self::resize_explorer))
                    .when(previewing, |this| this.child(div().flex().flex_col().flex_1().min_w_0().min_h_0().child(self.preview.clone())))
                    .when(show_explorer, |this| this.child(self.render_explorer_pane(previewing, cx)))
                    .child({
                        let body_width = self.body_width.clone();
                        canvas(move |bounds, _, _| body_width.set(f32::from(bounds.size.width)), |_, _, _, _| {}).absolute().size_full()
                    }),
            )
            .into_any_element()
    }
}
