//! Markdown link handling (thread links, workspace/host file links, citation popovers, external
//! preview/PR routing) and fenced shell execution in the thread terminal.

use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{App, ClickEvent, Entity, EntityId, Global, Pixels, Point, Window};
use t3_core::assistant_citations::Citation;
use t3_core::client_settings::BrowserLinkTarget;
use t3_core::terminal::method as terminal_method;
use t3_core::markdown::links::{FileLinkMediaKind, MarkdownLinkKind, ResolvedFileLink, ThreadLinkTarget, classify_link};
use t3_core::project_scripts;
use t3_core::project_search::{OPEN_IN_EDITOR, open_in_editor_request};
use t3_core::sidebar::tag_filter::ThreadKey;

use crate::app_state::{AppEvent, AppState, ScopedRouteAction};
use crate::toast::{Toast, ToastKind};

static TERMINAL_RUN_SEQ: AtomicU64 = AtomicU64::new(1);

/// Shared context attached to transcript Markdown views so link clicks, shell runs, and citation
/// popovers can drive `AppState` and notify the owning view.
#[derive(Clone)]
pub struct MarkdownActionContext {
    pub state: Entity<AppState>,
    pub owner: EntityId,
    pub cwd: Option<String>,
    pub thread_id: Option<String>,
    pub thread_view: Option<gpui::WeakEntity<crate::thread_view::ThreadView>>,
}

/// Active sent-citation detail popover opened from a `t3-citation://v1/...` link or chip.
#[derive(Clone)]
pub struct SentCitationPopover {
    pub citation: Citation,
    pub position: Point<Pixels>,
    pub state: Entity<AppState>,
    pub owner: EntityId,
}

#[derive(Default)]
pub struct CitationPopoverGlobal {
    pub active: Option<SentCitationPopover>,
}

impl Global for CitationPopoverGlobal {}

pub fn open_sent_citation_popover(
    citation: Citation,
    position: Point<Pixels>,
    state: Entity<AppState>,
    owner: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    cx.set_global(CitationPopoverGlobal {
        active: Some(SentCitationPopover { citation, position, state, owner }),
    });
    cx.notify(owner);
    window.refresh();
}

pub fn close_sent_citation_popover(cx: &mut App) {
    let owner = cx
        .try_global::<CitationPopoverGlobal>()
        .and_then(|g| g.active.as_ref().map(|a| a.owner));
    if let Some(owner) = owner {
        cx.set_global(CitationPopoverGlobal { active: None });
        cx.notify(owner);
    }
}

/// Dispatches clicks on Markdown hyperlinks and linked inline-code file paths.
pub fn handle_markdown_link_click(
    url: &str,
    event: &ClickEvent,
    action_ctx: Option<&MarkdownActionContext>,
    window: &mut Window,
    cx: &mut App,
) {
    let cwd = action_ctx.and_then(|c| c.cwd.as_deref()).or_else(|| {
        action_ctx
            .and_then(|c| c.state.read(cx).file_workspace())
            .map(|w| w.cwd)
            .as_deref()
            .map(|_| "")
    });
    let resolved_cwd = if cwd == Some("") {
        action_ctx.and_then(|c| c.state.read(cx).file_workspace()).map(|w| w.cwd)
    } else {
        cwd.map(str::to_owned)
    };

    match classify_link(url, resolved_cwd.as_deref()) {
        MarkdownLinkKind::Citation(citation) => {
            if let Some(ctx) = action_ctx {
                if event.modifiers().platform {
                    navigate_to_citation_source(citation, &ctx.state, cx);
                } else {
                    open_sent_citation_popover(citation, event.position(), ctx.state.clone(), ctx.owner, window, cx);
                }
            }
        }
        MarkdownLinkKind::Thread(target) => {
            if let Some(ctx) = action_ctx {
                open_thread_link(&target, &ctx.state, cx);
            }
        }
        MarkdownLinkKind::File(file) => {
            if let Some(ctx) = action_ctx {
                open_file_link(&file, event, ctx, cx);
            } else if file.media_kind == FileLinkMediaKind::Pdf || file.full_path.starts_with('/') {
                cx.open_url(&file_url(&file.full_path));
            }
        }
        MarkdownLinkKind::External(web) => {
            open_external_link(&web.url, action_ctx, cx);
        }
        MarkdownLinkKind::Anchor(_) | MarkdownLinkKind::Unsupported => {}
    }
}

/// Navigates to a `t3-thread://` or hosted `app.t3.codes` thread link, switching environments when
/// needed (`v1` cross-env fallback supported).
pub fn open_thread_link(target: &ThreadLinkTarget, state: &Entity<AppState>, cx: &mut App) {
    state.update(cx, |app, cx| {
        let active_canonical = app
            .server_config
            .config
            .as_ref()
            .map(|c| c.environment.environment_id.as_str());
        let active_saved = app.environments.active.as_deref();

        if let Some(env_id) = target.environment_id.as_deref() {
            if active_canonical == Some(env_id) || active_saved == Some(env_id) {
                if app.thread_shell(&target.thread_id).is_some() {
                    app.select_thread(&target.thread_id, cx);
                } else {
                    app.open_related_thread(&target.thread_id, cx);
                }
                return;
            }
            if let Some(entry) = app.scoped_environment(env_id) {
                let owner = entry.summary.owner.clone();
                if app.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE) {
                    app.navigate_scoped(owner, ScopedRouteAction::Thread(target.thread_id.clone()), cx);
                    return;
                }
            }
            let prev_selected = app.selected_thread.clone();
            app.open_linked_thread(env_id, &target.thread_id, cx);
            if app.selected_thread != prev_selected || app.has_pending_scoped_route() {
                return;
            }
        }

        if app.thread_shell(&target.thread_id).is_some() {
            app.select_thread(&target.thread_id, cx);
            return;
        }

        if target.allow_cross_env_fallback {
            let matching_owner = app
                .fleet
                .environments
                .iter()
                .find(|entry| {
                    app.scoped_ready(&entry.summary.owner, t3_core::auth::ORCHESTRATION_READ_SCOPE)
                        && app.scoped_thread(&ThreadKey::new(&entry.summary.owner.saved_id, &target.thread_id)).is_some()
                })
                .map(|entry| entry.summary.owner.clone());
            if let Some(owner) = matching_owner {
                app.navigate_scoped(owner, ScopedRouteAction::Thread(target.thread_id.clone()), cx);
                return;
            }
        }

        app.open_related_thread(&target.thread_id, cx);
    });
}

fn resolve_action_target(ctx: &MarkdownActionContext, app: &AppState) -> (Option<String>, Option<String>) {
    let thread_id = ctx.thread_id.clone().or_else(|| app.selected_thread.clone());
    let cwd = ctx
        .cwd
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            thread_id.as_deref().and_then(|id| {
                let thread = app.thread_shell(id)?;
                let project = app.shell.project(&thread.project_id)?;
                let root = thread.worktree_path.clone().unwrap_or_else(|| project.workspace_root.clone());
                (!root.is_empty()).then_some(root)
            })
        })
        .or_else(|| app.file_workspace().map(|w| w.cwd));
    (thread_id, cwd)
}

fn open_file_link(file: &ResolvedFileLink, event: &ClickEvent, ctx: &MarkdownActionContext, cx: &mut App) {
    let state = &ctx.state;
    if event.modifiers().alt {
        reveal_or_open_in_editor(ctx, file, true, cx);
        return;
    }
    if event.modifiers().platform {
        reveal_or_open_in_editor(ctx, file, false, cx);
        return;
    }
    if file.media_kind == FileLinkMediaKind::Pdf {
        let target_thread = {
            let app = state.read(cx);
            resolve_action_target(ctx, app).0
        };
        if let Some(previews) = crate::preview::previews(cx)
            && let Some(thread_id) = target_thread
        {
            previews.update(cx, |p, cx| p.open_link(thread_id, file_url(&file.full_path), cx));
        } else {
            cx.open_url(&file_url(&file.full_path));
        }
        return;
    }
    let (panel_open, action_cwd) = {
        let app = state.read(cx);
        (app.panel_thread().is_some(), resolve_action_target(ctx, app).1)
    };
    if panel_open && let Some(cwd) = action_cwd {
        let target = t3_core::files::file_surface_path(&file.full_path, Some(&cwd));
        state.update(cx, |app, cx| {
            app.mutate_panels(cx, |panels, thread| panels.panels.open_file(thread, target.as_deref(), file.line));
        });
    } else if file.full_path.starts_with('/') {
        cx.open_url(&file_url(&file.full_path));
    }
}

fn reveal_or_open_in_editor(ctx: &MarkdownActionContext, file: &ResolvedFileLink, reveal: bool, cx: &mut App) {
    let state = &ctx.state;
    let cwd = {
        let app = state.read(cx);
        resolve_action_target(ctx, app).1.unwrap_or_else(|| "/".to_owned())
    };
    let path_for_editor = file.relative_path.as_deref().unwrap_or(&file.full_path);

    if reveal {
        let can_reveal = state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .is_some_and(|cfg| cfg.available_editors.iter().any(|e| e == "file-manager"));
        if can_reveal {
            let payload = open_in_editor_request(&file.full_path, "file-manager");
            let runtime = state.read(cx).runtime().clone();
            if let Some(client) = state.read(cx).client_handle() {
                runtime.spawn(async move {
                    let _ = client.request(OPEN_IN_EDITOR, payload).await;
                });
                return;
            }
        }
        if let Some(parent) = std::path::Path::new(&file.full_path).parent() {
            cx.open_url(&file_url(&parent.to_string_lossy()));
        }
        return;
    }

    let opened = state.update(cx, |app, cx| app.open_in_editor(&cwd, path_for_editor, file.line, cx));
    if !opened && file.full_path.starts_with('/') {
        cx.open_url(&file_url(&file.full_path));
    }
}

fn open_external_link(url: &str, action_ctx: Option<&MarkdownActionContext>, cx: &mut App) {
    let settings = crate::settings::client_settings(cx);
    if settings.pull_request_links_open_in_app()
        && let Some(ctx) = action_ctx
    {
        let app = ctx.state.read(cx);
        let environment = app.environment();
        let target_thread = resolve_action_target(ctx, app).0;
        let project = target_thread
            .as_deref()
            .and_then(|id| app.thread_shell(id))
            .map(|t| t.project_id.as_str())
            .or(app.draft_project.as_deref());
        let projects = crate::pull_requests::link_projects(app);
        if let Some(target) = t3_core::pull_requests::target::resolve(url, &projects, environment, project, environment, true) {
            crate::pull_requests::open_resolved_link(&ctx.state, target, cx);
            return;
        }
    }

    if settings.browser_link_target() == BrowserLinkTarget::App
        && let Some(previews) = crate::preview::previews(cx)
        && let Some(thread_id) = action_ctx.and_then(|c| c.thread_id.clone().or_else(|| c.state.read(cx).selected_thread.clone()))
    {
        previews.update(cx, |p, cx| p.open_link(thread_id, url.to_owned(), cx));
        return;
    }

    cx.open_url(url);
}

/// Routes to a saved citation's source message (either on the active environment or cross-env).
pub fn navigate_to_citation_source(citation: Citation, state: &Entity<AppState>, cx: &mut App) {
    close_sent_citation_popover(cx);
    let can_switch = state.update(cx, |app, cx| {
        let same_env = citation.environment_id.is_empty()
            || app.environment() == Some(citation.environment_id.as_str())
            || app.environments.active.as_deref() == Some(citation.environment_id.as_str());
        if same_env {
            app.citation_source = Some(citation.clone());
            if app.selected_thread.as_deref() != Some(citation.thread_id.as_str()) {
                if app.thread_shell(&citation.thread_id).is_some() {
                    app.select_thread(&citation.thread_id, cx);
                } else {
                    app.open_related_thread(&citation.thread_id, cx);
                }
            }
            cx.emit(AppEvent::CitationSource);
            return true;
        }
        let target_owner = app
            .fleet
            .environments
            .iter()
            .find(|entry| {
                entry.summary.owner.saved_id == citation.environment_id
                    || entry.summary.owner.canonical_id.as_deref() == Some(citation.environment_id.as_str())
            })
            .map(|entry| entry.summary.owner.clone());
        if let Some(owner) = target_owner
            && app.scoped_ready(&owner, t3_core::auth::ORCHESTRATION_READ_SCOPE)
        {
            app.citation_source = Some(citation.clone());
            app.navigate_scoped(owner, ScopedRouteAction::Thread(citation.thread_id.clone()), cx);
            return true;
        }
        false
    });
    if !can_switch {
        state.update(cx, |app, cx| {
            app.show_toast(
                Toast::new(ToastKind::Error, "Source environment is unavailable")
                    .description("Connect the cited environment to jump to its message.".to_owned()),
                cx,
            );
        });
    }
}

/// Runs a closed shell fence in the active thread's terminal drawer (`Run in terminal`).
pub fn run_shell_code_block(command: &str, action_ctx: Option<&MarkdownActionContext>, window: &mut Window, cx: &mut App) {
    let Some(ctx) = action_ctx else { return };
    let (thread_id, cwd, client, runtime) = {
        let app = ctx.state.read(cx);
        let (Some(tid), Some(cwd)) = resolve_action_target(ctx, app) else {
            ctx.state.update(cx, |app, cx| {
                app.show_toast(Toast::new(ToastKind::Error, "Open a project thread to run shell commands"), cx);
            });
            return;
        };
        let Some(client) = app.client_handle() else { return };
        (tid, cwd, client, app.runtime().clone())
    };

    let seq = TERMINAL_RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    let terminal_id = format!("md-run-{seq}");
    let open = project_scripts::open_terminal(&thread_id, &terminal_id, &cwd, None);
    let write = project_scripts::write_terminal(&thread_id, &terminal_id, &project_scripts::terminal_input(command));
    window.dispatch_action(Box::new(crate::worktree_setup::OpenSetupTerminal { terminal_id }), cx);

    let state = ctx.state.clone();
    let task = runtime.spawn(async move {
        client.request(terminal_method::OPEN, open).await?;
        client.request(terminal_method::WRITE, write).await
    });
    cx.spawn(async move |cx| {
        if let Err(error) = task.await.unwrap_or_else(|_| Err(t3_client::session::RpcError::Closed)) {
            state.update(cx, |app, cx| {
                app.show_toast(Toast::new(ToastKind::Error, "Failed to run command in terminal").description(error.to_string()), cx);
            });
        }
    })
    .detach();
}

fn file_url(path: &str) -> String {
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{}", path.replace('\\', "/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext;
    use t3_core::shell::{ShellSnapshot, ShellState};

    #[gpui::test]
    fn resolve_action_target_prefers_side_chat_thread_and_cwd_over_selected_thread(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });
        state.update(cx, |app, _| {
            let projects = [("p-primary", "/tmp/primary-root"), ("p-side", "/tmp/side-root")]
                .into_iter()
                .map(|(id, path)| {
                    serde_json::from_value(serde_json::json!({
                        "id": id,
                        "title": id,
                        "workspaceRoot": path,
                        "createdAt": "2026-01-01T00:00:00Z",
                        "updatedAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap()
                })
                .collect();
            let threads = [("t-primary", "p-primary", "/tmp/primary-wt"), ("t-side", "p-side", "/tmp/side-shell-wt")]
                .into_iter()
                .map(|(id, project_id, wt)| {
                    serde_json::from_value(serde_json::json!({
                        "id": id,
                        "projectId": project_id,
                        "title": id,
                        "createdAt": "2026-01-01T00:00:00Z",
                        "updatedAt": "2026-01-01T00:00:00Z",
                        "lastVisitedAt": null,
                        "providerInstanceId": "replay",
                        "modelSelection": {"instanceId": "replay", "model": "replay"},
                        "runtimeMode": "auto",
                        "interactionMode": "default",
                        "worktreePath": wt,
                        "lineage": {"rootThreadId": id},
                        "status": "idle",
                        "hasActionableProposedPlan": false,
                        "itemCount": 0,
                        "visibleItemCount": 0
                    }))
                    .unwrap()
                })
                .collect();
            let mut shell = ShellState::default();
            shell.snapshot = Some(ShellSnapshot {
                projects,
                threads,
                ..Default::default()
            });
            app.shell = shell;
            app.selected_thread = Some("t-primary".into());
        });

        cx.update(|cx| {
            let app = state.read(cx);
            let owner = state.entity_id();
            let explicit_ctx = MarkdownActionContext {
                state: state.clone(),
                owner,
                thread_id: Some("t-side".into()),
                cwd: Some("/tmp/side-explicit".into()),
                thread_view: None,
            };
            assert_eq!(
                resolve_action_target(&explicit_ctx, app),
                (Some("t-side".into()), Some("/tmp/side-explicit".into()))
            );

            let thread_only_ctx = MarkdownActionContext {
                state: state.clone(),
                owner,
                thread_id: Some("t-side".into()),
                cwd: None,
                thread_view: None,
            };
            assert_eq!(
                resolve_action_target(&thread_only_ctx, app),
                (Some("t-side".into()), Some("/tmp/side-shell-wt".into()))
            );

            let fallback_ctx = MarkdownActionContext {
                state: state.clone(),
                owner,
                thread_id: None,
                cwd: None,
                thread_view: None,
            };
            assert_eq!(
                resolve_action_target(&fallback_ctx, app),
                (Some("t-primary".into()), Some("/tmp/primary-wt".into()))
            );
        });
    }
}
