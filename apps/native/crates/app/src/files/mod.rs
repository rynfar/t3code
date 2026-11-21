//! The file surfaces of the right panel. Ports `components/files/FilePreviewPanel.tsx` (the
//! breadcrumb subheader and its toggles, the preview beside a resizable explorer, folders that
//! show the explorer instead), `FileExplorerPane.tsx` and the refresh-on-mutation wiring of
//! `useWorkspaceMutationRefresh`. Listing and reading run on the network runtime; parsing and
//! decoding run on the background executor; views paint only what is on screen.

mod actions;
mod attachment;
mod audio;
mod binary;
pub(crate) mod code;
mod editor;
mod explorer;
mod explorer_rows;
mod font;
mod image;
mod mermaid;
mod notebook;
mod office;
mod pdf;
mod preview;
mod sqlite;
mod table;
#[cfg(feature = "snapshot")]
pub(crate) use preview::MarkdownFixture;

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
use t3_core::files::{breadcrumbs, group_thousands, is_host_path};
use t3_core::right_panel::{FileAttachment, SurfaceKind, breadcrumb_path_of, retained_breadcrumb_trail};

pub use actions::FileWorkspace;
pub(crate) use explorer_rows::file_icon;

pub fn init(cx: &mut App) {
    explorer::init(cx);
    editor::init(cx);
}

use crate::app_state::{AppEvent, AppState, Phase};
use crate::icons::Icon;
use crate::right_panel::{Button, Variant};
use crate::theme::{hsla, palette};
use explorer::{Explorer, ExplorerEvent};
use preview::{Preview, RenderedMode};

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

/// `t3code.fileExplorerOpen`, `t3code.fileExplorerWidth`, `t3code.renderMarkdown`,
/// `t3code.renderMermaid`, `t3code.renderTable`, and `t3code.renderNotebook`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Prefs {
    explorer_open: bool,
    explorer_width: f32,
    render_markdown: bool,
    render_mermaid: bool,
    render_table: bool,
    render_notebook: bool,
    render_html: bool,
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
            render_mermaid: value.get("renderMermaid").and_then(Value::as_bool).unwrap_or(true),
            render_table: value.get("renderTable").and_then(Value::as_bool).unwrap_or(true),
            render_notebook: value.get("renderNotebook").and_then(Value::as_bool).unwrap_or(true),
            render_html: value.get("renderBrowserFile").and_then(Value::as_bool).unwrap_or(true),
        }
    }

    fn save(&self) {
        store::save_ui_state(
            STORE,
            &json!({
                "explorerOpen": self.explorer_open,
                "explorerWidth": self.explorer_width,
                "renderMarkdown": self.render_markdown,
                "renderMermaid": self.render_mermaid,
                "renderTable": self.render_table,
                "renderNotebook": self.render_notebook,
                "renderBrowserFile": self.render_html,
            }),
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
    attachment: Option<FileAttachment>,
    folder_trail: Option<String>,
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
    wrap_override: Option<bool>,
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
        cx.subscribe(&explorer, |this, _, event: &ExplorerEvent, cx| match event {
            ExplorerEvent::Refresh => this.preview.update(cx, |preview, cx| preview.refetch(cx)),
            ExplorerEvent::FileOpened => this.preview.update(cx, |preview, cx| preview.request_content_focus(cx)),
        })
        .detach();
        cx.observe(&preview, |this, preview, cx| {
            // A path that reads as a folder opens in the explorer (`FileBrowserPanel` reveal).
            if preview.read(cx).is_folder()
                && let Some(path) = this.shown.as_ref().and_then(|shown| shown.path.clone())
            {
                this.explorer.update(cx, |explorer, cx| explorer.expand_folder(&path, cx));
            }
            crate::right_panel::publish_file_tab_dirty(&this.state, this.shown.as_ref().and_then(|shown| shown.path.as_deref()), preview.read(cx).is_dirty(), cx);
            cx.notify();
        })
        .detach();
        let prefs = Prefs::load();
        preview.update(cx, |preview, cx| {
            preview.set_render_markdown(prefs.render_markdown, cx);
            preview.set_render_mermaid(prefs.render_mermaid, cx);
            preview.set_render_table(prefs.render_table, cx);
            preview.set_render_notebook(prefs.render_notebook, cx);
            preview.set_render_html(prefs.render_html, cx);
        });
        let mut panel = FilesPanel {
            state,
            explorer,
            preview,
            root: None,
            shown: None,
            prefs,
            wrap: true,
            wrap_override: None,
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
        let shown = state.panel_thread().as_deref().and_then(|thread| state.panels.panels.active_surface(thread)).and_then(|surface| match &surface.kind {
            SurfaceKind::Files { folder_trail } => Some(Shown {
                surface_id: surface.id.clone(),
                path: None,
                reveal_line: None,
                reveal_request_id: 0,
                attachment: None,
                folder_trail: folder_trail.clone(),
            }),
            SurfaceKind::File { relative_path, reveal_line, reveal_request_id, attachment, folder_trail } => Some(Shown {
                surface_id: surface.id.clone(),
                path: Some(relative_path.clone()),
                reveal_line: *reveal_line,
                reveal_request_id: *reveal_request_id,
                attachment: attachment.clone(),
                folder_trail: folder_trail.clone(),
            }),
            SurfaceKind::Diff
            | SurfaceKind::PullRequests
            | SurfaceKind::PullRequest { .. }
            | SurfaceKind::Issue(_)
            | SurfaceKind::Terminal { .. }
            | SurfaceKind::SideChat { .. }
            | SurfaceKind::Preview { .. }
            | SurfaceKind::Device { .. } => None,
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
        let attachment = shown.as_ref().and_then(|shown| shown.attachment.clone())
            .zip(state.environment().map(str::to_owned))
            .map(|(file, environment)| attachment::AttachmentTarget { environment, file });
        let root_changed = root != self.root;
        if root_changed {
            self.root = root.clone();
            self.stale = false;
        } else if t3_core::media::workspace_mutation_changed(root_changed, self.mutation.as_deref(), mutation.as_deref()) {
            self.stale = true;
        }
        self.mutation = mutation;
        self.reconnected |= connected && !self.connected;
        self.connected = connected;
        if !visible && !root_changed && crate::right_panel::holding_file_surface(&self.state, cx) {
            return;
        }
        // Hidden files do no work: they catch up when they show.
        if visible {
            self.explorer.update(cx, |explorer, cx| explorer.set_root(root.clone(), cx));
            let target = shown.as_ref().filter(|shown| shown.attachment.is_none()).zip(root.as_ref()).and_then(|(shown, root)| {
                Some(preview::Target {
                    root: root.clone(),
                    path: shown.path.clone()?,
                    reveal_line: shown.reveal_line,
                    reveal_request_id: shown.reveal_request_id,
                })
            });
            self.preview.update(cx, |preview, cx| {
                preview.set_mutation(self.mutation.clone(), cx);
                if attachment.is_some() {
                    preview.set_attachment(attachment, cx);
                } else {
                    preview.set_target(target, cx);
                }
            });
            if std::mem::take(&mut self.stale) {
                self.explorer.update(cx, |explorer, cx| explorer.refresh_entries(cx));
                self.preview.update(cx, |preview, cx| preview.refetch(cx));
            }
            if std::mem::take(&mut self.reconnected) {
                self.explorer.update(cx, |explorer, cx| explorer.reconnected(cx));
                self.preview.update(cx, |preview, cx| preview.reconnected(cx));
            }
            let selected = shown.as_ref().and_then(|shown| shown.path.clone()).filter(|path| !is_host_path(path));
            let reveal = shown.as_ref().map_or(0, |shown| shown.reveal_request_id);
            self.explorer.update(cx, |explorer, cx| explorer.set_selected(selected, reveal, cx));
        } else if self.shown.as_ref().is_some_and(|shown| shown.attachment.is_some()) {
            self.preview.update(cx, |preview, cx| preview.set_attachment(None, cx));
        }
        if shown != self.shown || root_changed {
            self.shown = shown;
            cx.notify();
        }
    }

    fn set_prefs(&mut self, change: impl FnOnce(&mut Prefs), cx: &mut Context<Self>) {
        change(&mut self.prefs);
        self.prefs.save();
        let prefs = self.prefs;
        self.preview.update(cx, |preview, cx| {
            preview.set_render_markdown(prefs.render_markdown, cx);
            preview.set_render_mermaid(prefs.render_mermaid, cx);
            preview.set_render_table(prefs.render_table, cx);
            preview.set_render_notebook(prefs.render_notebook, cx);
            preview.set_render_html(prefs.render_html, cx);
        });
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
        let current_canonical = breadcrumb_path_of(path);
        let trail = retained_breadcrumb_trail(self.shown.as_ref().and_then(|shown| shown.folder_trail.as_deref()), path);
        let crumbs = breadcrumbs(project, &trail);
        let current_index = crumbs
            .iter()
            .position(|crumb| crumb.path == current_canonical)
            .unwrap_or_else(|| crumbs.len().saturating_sub(1));
        let crumb_row = h_flex().id("file-breadcrumbs").min_w_0().flex_1().overflow_x_scroll().text_xs().children(crumbs.into_iter().enumerate().flat_map(|(index, crumb)| {
            let label: SharedString = crumb.label.clone().into();
            let separator = (index > 0).then(|| Icon::ChevronRight.svg().flex_none().size(px(12.)).mx(px(2.)).text_color(hsla(p.muted_foreground).opacity(if index > current_index { 0.35 } else { 0.6 })).into_any_element());
            let state = self.state.clone();
            let surface_id = surface_id.clone();
            let folder = crumb.path.clone();
            let trail_for_click = trail.clone();
            let current = index == current_index;
            let descendant = index > current_index;
            let item = div()
                .id(("file-crumb", index))
                .flex_none()
                .px(px(4.))
                .py(px(2.))
                .rounded(px(4.))
                .whitespace_nowrap()
                .when(current, |this| this.font_weight(FontWeight::MEDIUM).text_color(hsla(p.foreground)))
                .when(!current, |this| {
                    this.text_color(hsla(p.muted_foreground).opacity(if descendant { 0.6 } else { 1. }))
                        .cursor_pointer()
                        .hover(|style| style.bg(hsla(p.accent)).text_color(hsla(p.foreground)))
                        .on_click(move |_, _, cx| {
                            state.update(cx, |state, cx| {
                                state.navigate_file_folder_with_trail(&surface_id, &folder, &trail_for_click, !is_folder, cx);
                            });
                        })
                })
                .child(label)
                .into_any_element();
            separator.into_iter().chain([item]).collect::<Vec<_>>()
        }));
        let preview_ref = self.preview.read(cx);
        let rendered_mode = previewing.then(|| preview_ref.rendered_mode()).flatten();
        let rendered = previewing && preview_ref.is_rendered_active();
        let raw_text = previewing && preview_ref.shows_text();
        let editing = previewing && preview_ref.is_editing();
        let dirty = previewing && preview_ref.is_dirty();
        let can_edit = previewing && preview_ref.can_edit_current(cx);
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
            .when(previewing, |this| this.children(self.open_in_picker(path, cx)))
            .when_some(rendered_mode, |this, mode| this.child(self.render_mode_toggle(mode, rendered, cx)))
            .when(can_edit, |this| {
                this.child(
                    Button::new("file-edit-toggle")
                        .variant(Variant::Toggle)
                        .icon(if editing { Icon::Eye } else { Icon::Pencil })
                        .icon_size(14.)
                        .pressed(editing || dirty)
                        .tooltip(if editing {
                            "Exit edit mode"
                        } else if dirty {
                            "Resume editing (unsaved changes)"
                        } else {
                            "Edit file"
                        })
                        .on_click(toggle(|this, cx| {
                            this.preview.update(cx, |preview, cx| {
                                if preview.is_editing() {
                                    preview.request_exit_edit_mode(cx);
                                } else {
                                    preview.enter_edit_mode(cx);
                                }
                            });
                            cx.notify();
                        })),
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
                            this.wrap_override = Some(this.wrap);
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
    fn render_mode_toggle(&self, mode: RenderedMode, rendered: bool, cx: &mut Context<Self>) -> Button {
        let tooltip = match (mode, rendered) {
            (RenderedMode::Markdown, true) => "Show markdown source",
            (RenderedMode::Markdown, false) => "Show rendered markdown",
            (RenderedMode::Mermaid, true) => "Show Mermaid source",
            (RenderedMode::Mermaid, false) => "Show rendered diagram",
            (RenderedMode::Table, true) => "Show source",
            (RenderedMode::Table, false) => "Show table",
            (RenderedMode::Notebook, true) => "Show notebook JSON",
            (RenderedMode::Notebook, false) => "Show rendered notebook",
            (RenderedMode::Html, true) => "Show HTML source",
            (RenderedMode::Html, false) => "Show rendered page",
        };
        Button::new("file-render-toggle")
            .variant(Variant::Toggle)
            .icon(if rendered { Icon::CodeXml } else if mode == RenderedMode::Table { Icon::Rows3 } else { Icon::Eye })
            .icon_size(14.)
            .pressed(rendered)
            .tooltip(crate::i18n::t(cx, tooltip))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.preview.update(cx, |preview, cx| preview.request_exit_edit_mode(cx));
                if this.preview.read(cx).is_editing() {
                    return;
                }
                this.set_prefs(|prefs| {
                    let render = match mode {
                        RenderedMode::Markdown => &mut prefs.render_markdown,
                        RenderedMode::Mermaid => &mut prefs.render_mermaid,
                        RenderedMode::Table => &mut prefs.render_table,
                        RenderedMode::Notebook => &mut prefs.render_notebook,
                        RenderedMode::Html => &mut prefs.render_html,
                    };
                    *render = !rendered;
                }, cx);
            }))
    }

    fn open_in_picker(&self, path: &str, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let app = self.state.read(cx);
        if !app.should_show_open_in_picker(self.root.is_some()) {
            return None;
        }
        let editors = actions::file_editors(app, cx);
        let preferred = preferred_file_editor(&editors, cx);
        let disabled = preferred.is_none();
        let root = self.root.as_ref()?.clone();
        let path = path.to_owned();
        let reveal = self.shown.as_ref().and_then(|shown| shown.reveal_line);
        let state = self.state.clone();
        let open = move |editor: &str, cx: &mut App| {
            state.update(cx, |state, cx| state.open_file_in_editor(&root, &path, reveal, editor, cx));
        };
        let open_favorite = open.clone();
        let label = crate::i18n::t(cx, "Open in");
        let options = t3_core::vcs::editor_options(&editors).into_iter().map(|editor| {
            let name = t3_core::vcs::editor_label(editor, cfg!(target_os = "macos"));
            crate::settings::controls::SelectOption::new(editor, name)
        }).collect();
        let picker = crate::settings::controls::Select::new("file-open-in-menu", options)
            .label(crate::i18n::t(cx, "Choose editor"))
            .disabled(disabled)
            .show_check()
            .on_change(move |editor, _, cx| open(editor.as_ref(), cx));
        Some(h_flex()
            .gap(px(2.))
            .child(Button::new("file-open-in")
                .icon(Icon::SquareArrowOutUpRight)
                .tooltip(label)
                .disabled(disabled)
                .on_click(move |_, _, cx| {
                    if let Some(editor) = preferred {
                        open_favorite(editor, cx);
                    }
                }))
            .child(match preferred {
                Some(editor) => picker.selected(editor),
                None => picker,
            })
            .into_any_element())
    }

    fn open_in_favorite(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((shown, root)) = self.shown.as_ref().zip(self.root.as_ref()) else { return false };
        let Some(path) = &shown.path else { return false };
        if shown.attachment.is_some() || self.preview.read(cx).is_folder() {
            return false;
        }
        let app = self.state.read(cx);
        if !app.should_show_open_in_picker(true) {
            return false;
        }
        let editors = actions::file_editors(app, cx);
        let Some(editor) = preferred_file_editor(&editors, cx) else { return false };
        self.state.update(cx, |state, cx| state.open_file_in_editor(root, path, shown.reveal_line, editor, cx));
        true
    }

    fn render_attachment(&self, attachment: &FileAttachment, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = palette(cx);
        let metadata = format!("{} · {} bytes", attachment.mime_type, group_thousands(attachment.size_bytes));
        let open_target = attachment_open_target(attachment);
        let preview = self.preview.read(cx);
        let mode = preview.rendered_mode();
        let rendered = preview.is_rendered_active();
        v_flex()
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .child(h_flex()
                .flex_none()
                .h(px(28.))
                .px(px(12.))
                .gap(px(6.))
                .items_center()
                .child(file_icon(&attachment.name).render(px(13.), hsla(p.muted_foreground)))
                .child(v_flex().flex_1().min_w_0()
                    .child(div().truncate().text_xs().font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground)).child(attachment.name.clone()))
                    .child(div().truncate().text_size(px(10.)).text_color(hsla(p.muted_foreground)).child(metadata)))
                .when_some(mode, |this, mode| this.child(self.render_mode_toggle(mode, rendered, cx)))
                .when_some(open_target, |this, target| {
                    this.child(div().debug_selector(|| "attachment-open-url".into())
                        .child(Button::new("attachment-open-url")
                            .icon(Icon::ExternalLink)
                            .tooltip(crate::i18n::t(cx, "Open attachment"))
                            .on_click(move |_, _, cx| match &target {
                                AttachmentOpenTarget::Url(url) => cx.open_url(url),
                                AttachmentOpenTarget::ServerAsset(attachment_id) => {
                                    crate::media::open_asset(
                                        &t3_core::media::AssetResource::Attachment { attachment_id: attachment_id.clone() },
                                        cx,
                                    );
                                }
                            })))
                }))
            .child(div().flex_1().min_h_0().child(self.preview.clone()))
            .into_any_element()
    }

}

fn preferred_file_editor(editors: &[String], cx: &App) -> Option<&'static str> {
    crate::thread_details::details(cx)
        .and_then(|details| details.read(cx).preferred_editor(editors))
        .or_else(|| t3_core::vcs::preferred_editor(None, editors))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AttachmentOpenTarget {
    Url(String),
    ServerAsset(String),
}

fn attachment_open_target(attachment: &FileAttachment) -> Option<AttachmentOpenTarget> {
    if let Some(url) = attachment.url.as_ref().filter(|u| u.starts_with("http://") || u.starts_with("https://")) {
        return Some(AttachmentOpenTarget::Url(url.clone()));
    }
    let id = attachment.id.trim();
    if !id.is_empty() {
        return Some(AttachmentOpenTarget::ServerAsset(id.to_owned()));
    }
    None
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wrap = self.wrap_override.unwrap_or(crate::theme::preferences(cx).typography.word_wrap);
        if self.wrap != wrap {
            self.wrap = wrap;
            self.preview.update(cx, |preview, cx| preview.set_wrap(wrap, cx));
        }
        if let Some(attachment) = self.shown.as_ref().and_then(|shown| shown.attachment.as_ref()).cloned() {
            return self.render_attachment(&attachment, cx);
        }
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
        let has_trail = self.shown.as_ref().and_then(|shown| shown.folder_trail.as_deref()).is_some_and(|t| !t.is_empty());
        let preview = self.preview.read(cx);
        let previewing = path.is_some() && !preview.is_folder();
        let notice = previewing.then(|| preview.truncation_notice()).flatten();
        let host = path.as_deref().is_some_and(is_host_path);
        let show_explorer = !host && (self.prefs.explorer_open || !previewing);
        let subheader_path = path.as_deref().or(has_trail.then_some(""));
        v_flex()
            .id("files-panel-root")
            .key_context("Preview")
            .size_full()
            .min_h_0()
            .bg(hsla(p.background))
            .on_action(cx.listener(move |this, _: &crate::preview::Find, window, cx| {
                // `pingdotgg/t3code#34346` & `#34360`: when browsing files with no file open,
                // panel Find focuses the explorer search; when previewing a file, focus the file's
                // find bar (PDF / code / editor), falling back to explorer search when the preview
                // has no in-file find bar.
                if !previewing {
                    this.explorer.update(cx, |explorer, cx| explorer.focus_search(window, cx));
                } else {
                    let handled = this.preview.update(cx, |preview, cx| preview.focus_find(window, cx));
                    if !handled && show_explorer {
                        this.explorer.update(cx, |explorer, cx| explorer.focus_search(window, cx));
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &crate::thread_details::OpenFavoriteEditor, _, cx| {
                if !this.open_in_favorite(cx) {
                    cx.propagate();
                }
            }))
            .children(subheader_path.map(|path| self.render_subheader(path, previewing, cx)))
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

#[cfg(all(test, feature = "snapshot"))]
mod tests {
    use super::*;

    #[test]
    fn attachment_open_target_falls_back_to_server_asset_when_url_absent() {
        let with_url = FileAttachment {
            id: "att-1".into(),
            name: "spec.pdf".into(),
            mime_type: "application/pdf".into(),
            size_bytes: 128,
            url: Some("https://example.com/spec.pdf".into()),
        };
        assert_eq!(
            attachment_open_target(&with_url),
            Some(AttachmentOpenTarget::Url("https://example.com/spec.pdf".into()))
        );

        let server_only = FileAttachment {
            id: "att-server-42".into(),
            name: "report.pdf".into(),
            mime_type: "application/pdf".into(),
            size_bytes: 256,
            url: None,
        };
        assert_eq!(
            attachment_open_target(&server_only),
            Some(AttachmentOpenTarget::ServerAsset("att-server-42".into()))
        );

        let unsafe_url_falls_back_to_id = FileAttachment {
            id: "att-server-43".into(),
            name: "note.txt".into(),
            mime_type: "text/plain".into(),
            size_bytes: 12,
            url: Some("javascript:alert(1)".into()),
        };
        assert_eq!(
            attachment_open_target(&unsafe_url_falls_back_to_id),
            Some(AttachmentOpenTarget::ServerAsset("att-server-43".into()))
        );

        let unopenable = FileAttachment {
            id: "   ".into(),
            name: "orphan.bin".into(),
            mime_type: "application/octet-stream".into(),
            size_bytes: 4,
            url: None,
        };
        assert_eq!(attachment_open_target(&unopenable), None);
    }

    #[gpui::test]
    fn server_backed_attachment_without_http_url_renders_open_attachment_button(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (state, _fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::keymap::init(cx);
            let installed = crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone());
            crate::settings::store::init(cx);
            installed
        });
        state.update(cx, |state, cx| {
            state.panels.panels = t3_core::right_panel::RightPanels::default();
            state.select_thread("menu-thread", cx);
            state.open_attachment(
                FileAttachment {
                    id: "att-server-99".into(),
                    name: "trace.log".into(),
                    mime_type: "text/plain".into(),
                    size_bytes: 512,
                    url: None,
                },
                cx,
            );
        });

        let (panel, vcx) = cx.add_window_view(|window, cx| FilesPanel::new(state.clone(), window, cx));
        vcx.update(|window, cx| {
            panel.update(cx, |panel, cx| panel.sync(cx));
            window.draw(cx).clear();
        });
        assert!(
            vcx.debug_bounds("attachment-open-url").is_some(),
            "server-backed attachment without HTTP URL must still render the Open attachment button"
        );

        // Empty ID + no URL renders no Open attachment button.
        state.update(vcx, |state, cx| {
            state.open_attachment(
                FileAttachment {
                    id: String::new(),
                    name: "orphan.bin".into(),
                    mime_type: "application/octet-stream".into(),
                    size_bytes: 4,
                    url: None,
                },
                cx,
            );
        });
        vcx.update(|window, cx| {
            panel.update(cx, |panel, cx| panel.sync(cx));
            window.draw(cx).clear();
        });
        assert!(
            vcx.debug_bounds("attachment-open-url").is_none(),
            "attachment with neither HTTP URL nor ID should omit the Open attachment button"
        );

        state.update(vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }

    #[gpui::test]
    async fn editor_saves_via_projects_write_file_and_explorer_drag_mentions_queue_to_composer(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (state, mut fixture) = cx.update(|cx| {
            gpui_component::init(cx);
            crate::keymap::init(cx);
            init(cx);
            crate::context_menu::project_filter_tests::install_state(cx, runtime.handle().clone())
        });
        fixture
            .grants
            .send(Some(
                serde_json::from_value(json!({
                    "authenticated": true,
                    "permissions": ["orchestration:read", "filesystem:read", "filesystem:write"]
                }))
                .unwrap(),
            ))
            .unwrap();

        let (panel, vcx) = cx.add_window_view(|window, cx| FilesPanel::new(state.clone(), window, cx));
        let make_target = |path: &str| preview::Target {
            root: Root {
                environment: "fixture-env".into(),
                workspace: FileWorkspace {
                    thread_id: "menu-thread".into(),
                    cwd: "/fixture/b".into(),
                    project_name: "Shared repo".into(),
                },
            },
            path: path.into(),
            reveal_line: None,
            reveal_request_id: 0,
        };
        let target_a = make_target("src/app.rs");
        let target_b = make_target("scripts/tool.py");
        let save_target_a = t3_core::files::save::FileSaveTarget::new("fixture-env", "/fixture/b", "src/app.rs");
        let save_target_b = t3_core::files::save::FileSaveTarget::new("fixture-env", "/fixture/b", "scripts/tool.py");

        let preview_entity = panel.read_with(vcx, |panel, _| panel.preview.clone());
        let explorer_entity = panel.read_with(vcx, |panel, _| panel.explorer.clone());

        // Start an in-flight write on target A (`src/app.rs`), then switch to target B (`scripts/tool.py`)
        // and start a second concurrent write before target A's reply returns.
        let editor_a_id = vcx.update(|window, cx| {
            panel.update(cx, |panel, cx| {
                panel.root = Some(target_a.root.clone());
                panel.shown = Some(Shown {
                    surface_id: "s-1".into(),
                    path: Some("src/app.rs".into()),
                    reveal_line: None,
                    reveal_request_id: 0,
                    attachment: None,
                    folder_trail: None,
                });
                cx.notify();
            });
            preview_entity.update(cx, |preview, cx| {
                preview.load_text_for_test(target_a.clone(), "fn before() {}\n", cx);
                preview.enter_edit_mode(cx);
                assert_eq!(preview.editor.language, Some("rust"));
                preview.on_editor_text_changed("fn after() {}\n".into(), cx);
                assert!(preview.is_dirty());
                preview.save_active_file(cx);
            });
            window.draw(cx).clear();
            preview_entity.read(cx).editor.editor.as_ref().expect("rust editor state").entity_id()
        });

        let (method_a, payload_a, reply_a) = loop {
            let req = fixture.next_request().await;
            if req.0 == t3_core::files::WRITE_FILE {
                break req;
            }
            let _ = req.2.send(Ok(json!({})));
        };
        assert_eq!(method_a, t3_core::files::WRITE_FILE);
        assert_eq!(
            payload_a,
            json!({ "cwd": "/fixture/b", "relativePath": "src/app.rs", "contents": "fn after() {}\n" })
        );

        // Switch to a Python file while target A's write is still in flight; verify language updates,
        // EditorState is recreated with the new grammar, and target B's write does not cancel target A's persist task.
        let editor_b_id = vcx.update(|window, cx| {
            preview_entity.update(cx, |preview, cx| {
                preview.release_active_editor_on_target_change(cx);
                preview.load_text_for_test(target_b.clone(), "print('before')\n", cx);
                preview.enter_edit_mode(cx);
                assert_eq!(preview.editor.language, Some("python"));
                preview.on_editor_text_changed("print('after')\n".into(), cx);
                preview.save_active_file(cx);
            });
            window.draw(cx).clear();
            preview_entity.read(cx).editor.editor.as_ref().expect("python editor state").entity_id()
        });
        assert_ne!(editor_a_id, editor_b_id, "switching files must recreate EditorState with updated language");

        let (method_b, payload_b, reply_b) = loop {
            let req = fixture.next_request().await;
            if req.0 == t3_core::files::WRITE_FILE {
                break req;
            }
            let _ = req.2.send(Ok(json!({})));
        };
        assert_eq!(method_b, t3_core::files::WRITE_FILE);
        assert_eq!(
            payload_b,
            json!({ "cwd": "/fixture/b", "relativePath": "scripts/tool.py", "contents": "print('after')\n" })
        );

        // Take both per-target persist tasks and resolve both RPCs; neither must deadlock in `saving`.
        let (task_a, task_b) = preview_entity.update(vcx, |preview, _| {
            (
                preview.editor.persist_tasks.remove(&save_target_a.key()).expect("target A persist task kept"),
                preview.editor.persist_tasks.remove(&save_target_b.key()).expect("target B persist task kept"),
            )
        });
        reply_a.send(Ok(json!({ "relativePath": "src/app.rs" }))).unwrap();
        reply_b.send(Ok(json!({ "relativePath": "scripts/tool.py" }))).unwrap();
        task_a.await;
        task_b.await;
        vcx.run_until_parked();

        preview_entity.read_with(vcx, |preview, _| {
            assert!(!preview.editor.store.is_saving(&save_target_a), "target A must not deadlock in saving");
            assert!(!preview.editor.store.is_pending(&save_target_a), "target A confirmed write must clear pending");
            assert!(!preview.editor.store.is_saving(&save_target_b), "target B must not deadlock in saving");
            assert!(!preview.is_dirty(), "confirmed write must clear dirty state");
            assert_eq!(preview.current_text_contents().as_deref(), Some("print('after')\n"));
        });

        // Aborted drag clears `drag_mention.is_drag_in_progress()` on render when no drag is active.
        vcx.update(|window, cx| {
            explorer_entity.update(cx, |explorer, _| {
                let _ = explorer.begin_drag_mention("src/app.rs").expect("drag mention payload");
                assert!(explorer.drag_mention.is_drag_in_progress());
            });
            window.draw(cx).clear();
        });
        explorer_entity.read_with(vcx, |explorer, _| {
            assert!(
                !explorer.drag_mention.is_drag_in_progress(),
                "aborted drag without active GPUI drag must clear drag-to-mention state"
            );
        });

        // Explorer drag-to-mention drop emits mention into AppState::composer_mentions.
        explorer_entity.update(vcx, |explorer, cx| {
            let dragged = explorer.begin_drag_mention("src/app.rs").expect("drag mention payload");
            assert_eq!(dragged.mention, "[app.rs](src/app.rs)");
            explorer.drop_mention_to_chat(&dragged, cx);
        });
        state.read_with(vcx, |state, _| {
            assert!(
                state.composer_mentions.iter().any(|m| m.path == "src/app.rs"),
                "dropped tree drag must queue composer mention"
            );
        });

        // Cmd+F in the Files explorer focuses its search input (`pingdotgg/t3code#34346`),
        // and opening a file moves focus to the editor (when editing) or preview content (`pingdotgg/t3code#34360`).
        vcx.update(|window, cx| {
            explorer_entity.update(cx, |explorer, cx| {
                explorer.focus_search(window, cx);
                assert!(
                    gpui::Focusable::focus_handle(explorer.search_input.read(cx), cx).is_focused(window),
                    "focus_search must focus the explorer search input"
                );
            });
            preview_entity.update(cx, |preview, cx| {
                preview.request_content_focus(cx);
            });
            window.draw(cx).clear();
            let editor_focus = gpui::Focusable::focus_handle(
                preview_entity
                    .read(cx)
                    .editor
                    .editor
                    .as_ref()
                    .expect("active editor")
                    .read(cx),
                cx,
            );
            assert!(editor_focus.is_focused(window), "opening a file in edit mode must focus its editor");

            // Exiting edit mode and requesting content focus focuses the read-only preview surface.
            preview_entity.update(cx, |preview, cx| {
                preview.request_exit_edit_mode(cx);
                assert!(!preview.is_editing());
                preview.request_content_focus(cx);
            });
            window.draw(cx).clear();
            let content_focus = preview_entity.read(cx).content_focus.clone();
            assert!(content_focus.is_focused(window), "opening a read-only file must focus preview content");

            // Dispatching `preview.find` on a non-searchable preview (e.g. RTF Office doc)
            // propagates out of `Preview` to `FilesPanel` and falls back to explorer search.
            preview_entity.update(cx, |preview, cx| {
                preview.load_office_for_test(t3_core::files::office::OfficeKind::Rtf, br"{\rtf1\ansi Hi}".to_vec(), cx);
                preview.request_content_focus(cx);
            });
            window.draw(cx).clear();
            let content_focus = preview_entity.read(cx).content_focus.clone();
            assert!(content_focus.is_focused(window));
            content_focus.dispatch_action(&crate::preview::Find, window, cx);
            let search_focus = gpui::Focusable::focus_handle(explorer_entity.read(cx).search_input.read(cx), cx);
            assert!(
                search_focus.is_focused(window),
                "preview.find on non-searchable preview must fall back to focusing explorer search"
            );
            // Blur before `TestWindow` repaints (`gpui-component` calls `set_text_content_type` on focused inputs).
            window.blur();
        });

        state.update(vcx, |state, _| state.stop_pump());
        cx.update(|cx| {
            cx.shutdown();
            cx.clear_globals();
        });
    }
}
