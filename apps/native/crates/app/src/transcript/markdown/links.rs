//! Markdown link handling (thread links, workspace/host file links, citation popovers, external
//! preview/PR routing) and fenced shell execution in the thread terminal.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{
    AnyElement, App, BorrowAppContext, ClickEvent, ClipboardItem, Entity, EntityId, FontWeight, Global,
    InteractiveElement, IntoElement, ParentElement, Pixels, Point, Styled, Window, div, px,
};
use gpui_component::ActiveTheme;
use serde_json::{Value, json};
use t3_core::assistant_citations::Citation;
use t3_core::client_settings::BrowserLinkTarget;
use t3_core::files::{SEARCH_ENTRIES, decode_search};
use t3_core::markdown::links::{
    AGENT_DIRECTORY_SEARCH_LIMIT, FileLinkMediaKind, MarkdownLinkKind, ResolvedFileLink, ThreadLinkTarget,
    WORKSPACE_BASENAME_LOOKUP_LIMIT, WorkspaceEntryCandidate, WorkspaceEntryKind, classify_link,
    is_macos_app_bundle_path, needs_workspace_basename_lookup, open_in_editor_request_with_root,
    pick_project_link_match, pick_workspace_link_match, resolve_full_and_relative, unknown_thread_link_label,
    workspace_link_lookup_query,
};
use t3_core::project_scripts;
use t3_core::project_search::{OPEN_IN_EDITOR, editor_target, preferred_editor};
use t3_core::sidebar::tag_filter::ThreadKey;
use t3_core::terminal::method as terminal_method;

use crate::app_state::{AppEvent, AppState, ScopedRouteAction};
use crate::icons::Icon;
use crate::theme::{hsla, palette};
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

/// Active `@model` mention detail popover opened from a `t3-model://` chip/link.
#[derive(Clone)]
pub struct ModelMentionPopover {
    pub label: String,
    pub provider_label: String,
    pub source: String,
    pub position: Point<Pixels>,
    pub owner: EntityId,
}

#[derive(Default)]
pub struct ModelMentionPopoverGlobal {
    pub active: Option<ModelMentionPopover>,
}

impl Global for ModelMentionPopoverGlobal {}

pub fn open_model_mention_popover(
    instance_id: &str,
    model: &str,
    position: Point<Pixels>,
    state: &Entity<AppState>,
    owner: EntityId,
    window: &mut Window,
    cx: &mut App,
) {
    let source = format!("model:{instance_id}/{model}");
    let (label, provider_label) = state
        .read(cx)
        .server_config
        .config
        .as_ref()
        .and_then(|cfg| {
            let provider = cfg.providers.iter().find(|p| p.instance_id == instance_id)?;
            let option = provider
                .models
                .iter()
                .find(|m| m.slug == model)
                .or_else(|| provider.models.iter().find(|m| m.slug.eq_ignore_ascii_case(model)))?;
            let label = option.short_name.as_deref().filter(|s| !s.is_empty()).unwrap_or(&option.name).to_owned();
            let provider_label = option
                .sub_provider
                .as_deref()
                .or(provider.display_name.as_deref())
                .unwrap_or(&provider.driver)
                .to_owned();
            Some((label, provider_label))
        })
        .unwrap_or_else(|| (model.to_owned(), instance_id.to_owned()));

    cx.set_global(ModelMentionPopoverGlobal {
        active: Some(ModelMentionPopover { label, provider_label, source, position, owner }),
    });
    cx.notify(owner);
    window.refresh();
}

pub fn close_model_mention_popover(cx: &mut App) {
    let owner = cx
        .try_global::<ModelMentionPopoverGlobal>()
        .and_then(|g| g.active.as_ref().map(|a| a.owner));
    if let Some(owner) = owner {
        cx.set_global(ModelMentionPopoverGlobal { active: None });
        cx.notify(owner);
    }
}

pub fn render_model_mention_popover(owner: EntityId, window: &Window, cx: &mut App) -> Option<AnyElement> {
    let active = cx
        .try_global::<ModelMentionPopoverGlobal>()
        .and_then(|g| g.active.as_ref())
        .filter(|a| a.owner == owner)?
        .clone();
    let p = palette(cx);
    let bounds = window.viewport_size();
    let x = f32::from(active.position.x).clamp(8., (f32::from(bounds.width) - 280.).max(8.));
    let y = (f32::from(active.position.y) + 12.).clamp(8., (f32::from(bounds.height) - 160.).max(8.));
    let copy_source = active.source.clone();

    Some(
        div()
            .id("model-mention-popover-layer")
            .absolute()
            .inset_0()
            .child(
                div()
                    .id("model-mention-popover")
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(268.))
                    .p(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(hsla(p.border))
                    .bg(hsla(p.popover))
                    .shadow_sm()
                    .occlude()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down_out(|_, _, cx| close_model_mention_popover(cx))
                    .on_key_down(|event: &gpui::KeyDownEvent, _, cx| {
                        if event.keystroke.key == "escape" {
                            close_model_mention_popover(cx);
                            cx.stop_propagation();
                        }
                    })
                    .child(
                        gpui_component::h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(hsla(p.foreground))
                                    .child(active.label),
                            )
                            .child(crate::transcript::parts::ghost_icon_button(
                                "close-model-mention",
                                Icon::X,
                                |_, _, cx| close_model_mention_popover(cx),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(hsla(p.muted_foreground))
                            .child(active.provider_label),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(hsla(p.muted_foreground))
                            .child(active.source),
                    )
                    .child(
                        gpui_component::h_flex().justify_end().pt(px(2.)).child(
                            crate::transcript::parts::ghost_text_button(
                                "copy-model-mention",
                                "Copy mention",
                                Some(Icon::Copy),
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy_source.clone()));
                                    close_model_mention_popover(cx);
                                },
                                cx,
                            ),
                        ),
                    ),
            )
            .into_any_element(),
    )
}

/// Dispatches clicks on Markdown links with optional row key context for `agentWorkingDirectories`.
pub fn handle_markdown_link_click_for_source(
    url: &str,
    event: &ClickEvent,
    action_ctx: Option<&MarkdownActionContext>,
    source_key: Option<&str>,
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
        MarkdownLinkKind::ModelMention { instance_id, model } => {
            if let Some(ctx) = action_ctx {
                open_model_mention_popover(&instance_id, &model, event.position(), &ctx.state, ctx.owner, window, cx);
            }
        }
        MarkdownLinkKind::Thread(target) => {
            if let Some(ctx) = action_ctx {
                open_thread_link(&target, &ctx.state, cx);
            }
        }
        MarkdownLinkKind::File(file) => {
            if let Some(ctx) = action_ctx {
                open_file_link_for_source(&file, event, ctx, source_key, cx);
            } else if file.media_kind == FileLinkMediaKind::Pdf || file.full_path.starts_with('/') {
                cx.open_url(&file_url(&file.full_path));
            }
        }
        MarkdownLinkKind::External(web) => {
            open_external_link(&web.url, event.modifiers(), action_ctx, cx);
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
                app.show_toast(Toast::new(ToastKind::Info, unknown_thread_link_label(false)), cx);
                return;
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

        if target.environment_id.is_some() {
            app.show_toast(Toast::new(ToastKind::Info, unknown_thread_link_label(false)), cx);
            return;
        }

        app.open_related_thread(&target.thread_id, cx);
    });
}

#[derive(Default)]
pub struct ThreadLinkMetadataGlobal {
    pub by_thread_id: HashMap<String, t3_core::markdown::links::ThreadLinkMetadata>,
    pub revision: u64,
}

impl Global for ThreadLinkMetadataGlobal {}

/// Fetches metadata (`title`, optional `projectId`) for unloaded linked threads via
/// `orchestration.getThreadLinkMetadata` (or `orchestration.getThreadTitles` fallback).
pub fn prefetch_thread_link_metadata(state: &Entity<AppState>, owner: EntityId, thread_ids: Vec<String>, cx: &mut App) {
    let missing: Vec<String> = {
        let cached = cx.try_global::<ThreadLinkMetadataGlobal>();
        let app = state.read(cx);
        thread_ids
            .into_iter()
            .filter(|id| app.thread_shell(id).is_none() && cached.is_none_or(|g| !g.by_thread_id.contains_key(id)))
            .collect()
    };
    if missing.is_empty() {
        return;
    }
    let Some(client) = state.read(cx).client_handle() else {
        return;
    };
    let supports_metadata = state
        .read(cx)
        .server_config
        .config
        .as_ref()
        .is_some_and(|c| c.thread_link_metadata == Some(true));
    let runtime = state.read(cx).runtime().clone();
    cx.spawn(async move |cx| {
        let task = runtime.spawn(async move {
            t3_client::environments::fetch_thread_link_metadata(&client, &missing, supports_metadata).await
        });
        if let Ok(Ok(map)) = task.await
            && !map.is_empty()
        {
            cx.update(|cx| {
                if !cx.has_global::<ThreadLinkMetadataGlobal>() {
                    cx.set_global(ThreadLinkMetadataGlobal::default());
                }
                cx.update_global::<ThreadLinkMetadataGlobal, _>(|global, _| {
                    global.by_thread_id.extend(map);
                    global.revision = global.revision.wrapping_add(1);
                });
                cx.notify(owner);
            });
        }
    })
    .detach();
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

#[derive(Clone)]
struct LinkClickAction {
    alt: bool,
    platform: bool,
}

fn open_file_link_for_source(
    file: &ResolvedFileLink,
    event: &ClickEvent,
    ctx: &MarkdownActionContext,
    source_key: Option<&str>,
    cx: &mut App,
) {
    let modifiers = event.modifiers();
    let click = LinkClickAction { alt: modifiers.alt, platform: modifiers.platform };
    if !click.alt
        && let Some(lookup_rel) = file.relative_path.as_deref().or_else(|| {
            (!file.full_path.starts_with('/') && !file.full_path.contains(":\\")).then_some(file.full_path.as_str())
        })
        && let Some(cwd) = {
            let app = ctx.state.read(cx);
            resolve_action_target(ctx, app).1
        }
        && let Some(query) = workspace_link_lookup_query(lookup_rel, &cwd)
    {
        resolve_and_open_workspace_file_link(file.clone(), lookup_rel.to_owned(), cwd, query, ctx.clone(), source_key.map(str::to_owned), click, cx);
        return;
    }
    open_resolved_file_now(file, None, &click, ctx, cx);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacosAppBundlePolicy {
    OpenViaFileManager,
    Blocked,
    NotAppBundle,
}

fn is_client_host(app: &AppState) -> bool {
    app.active_environment().is_some_and(|env| {
        env.ssh.is_none()
            && (env.local
                || app.local_saved_environment().is_some_and(|local| local.id == env.id)
                || t3_core::settings::providers::chatgpt::local_server(&env.http_base, false))
    })
}

fn can_operate_host(app: &AppState) -> bool {
    app.client_handle().is_some_and(|client| {
        client
            .session_state()
            .borrow()
            .as_ref()
            .is_some_and(|session| session.grants(t3_core::auth::ORCHESTRATION_OPERATE_SCOPE))
    })
}

fn macos_app_bundle_policy(app: &AppState, full_path: &str) -> MacosAppBundlePolicy {
    let Some(config) = app.server_config.config.as_ref() else {
        return MacosAppBundlePolicy::NotAppBundle;
    };
    if !config.environment.platform.as_ref().is_some_and(|p| p.os == "darwin") || !is_macos_app_bundle_path(full_path) {
        return MacosAppBundlePolicy::NotAppBundle;
    }
    let has_file_manager = config.available_editors.iter().any(|editor| editor == "file-manager");
    if is_client_host(app) && can_operate_host(app) && has_file_manager {
        MacosAppBundlePolicy::OpenViaFileManager
    } else {
        MacosAppBundlePolicy::Blocked
    }
}

fn open_resolved_file_now(
    file: &ResolvedFileLink,
    resolved_root: Option<&str>,
    click: &LinkClickAction,
    ctx: &MarkdownActionContext,
    cx: &mut App,
) {
    let state = &ctx.state;
    if !click.alt && !click.platform {
        match macos_app_bundle_policy(state.read(cx), &file.full_path) {
            MacosAppBundlePolicy::OpenViaFileManager => {
                let app = state.read(cx);
                let payload = open_in_editor_request_with_root(&file.full_path, "file-manager", None);
                let runtime = app.runtime().clone();
                if let Some(client) = app.client_handle() {
                    runtime.spawn(async move {
                        let _ = client.request(OPEN_IN_EDITOR, payload).await;
                    });
                }
                return;
            }
            MacosAppBundlePolicy::Blocked => return,
            MacosAppBundlePolicy::NotAppBundle => {}
        }
    }
    if click.alt {
        reveal_or_open_in_editor(ctx, file, resolved_root, true, cx);
        return;
    }
    if click.platform {
        reveal_or_open_in_editor(ctx, file, resolved_root, false, cx);
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
    let (panel_open, panel_cwd) = {
        let app = state.read(cx);
        (
            app.panel_thread().is_some(),
            app.file_workspace().map(|w| w.cwd).filter(|s| !s.is_empty()),
        )
    };
    if panel_open
        && (panel_cwd.is_some()
            || file.full_path.starts_with('/')
            || t3_core::markdown::links::is_windows_drive_path(&file.full_path))
    {
        let target = t3_core::files::file_surface_path(&file.full_path, panel_cwd.as_deref());
        state.update(cx, |app, cx| {
            app.mutate_panels(cx, |panels, thread| panels.panels.open_file(thread, target.as_deref(), file.line));
        });
    } else if file.full_path.starts_with('/') {
        cx.open_url(&file_url(&file.full_path));
    }
}

fn candidate_project_and_agent_roots(
    app: &AppState,
    thread_view: Option<&gpui::WeakEntity<crate::thread_view::ThreadView>>,
    target_thread_id: Option<&str>,
    source_key: Option<&str>,
    active_cwd: &str,
    cx: &App,
) -> (Vec<String>, Vec<String>) {
    let thread_project_id = target_thread_id
        .and_then(|id| app.thread_shell(id))
        .map(|thread| thread.project_id.as_str())
        .or(app.draft_project.as_deref());
    let mut other_roots = Vec::new();
    if let Some(snapshot) = app.shell.snapshot.as_ref() {
        for project in &snapshot.projects {
            if !project.workspace_root.is_empty()
                && Some(project.id.as_str()) != thread_project_id
                && project.workspace_root != active_cwd
                && !other_roots.contains(&project.workspace_root)
            {
                other_roots.push(project.workspace_root.clone());
            }
        }
    }
    let agent_roots: Vec<String> = thread_view
        .and_then(|tv| tv.upgrade())
        .map(|tv| {
            tv.read(cx)
                .agent_working_directories_for_key(source_key)
                .into_iter()
                .filter(|dir| dir != active_cwd)
                .take(AGENT_DIRECTORY_SEARCH_LIMIT)
                .collect()
        })
        .unwrap_or_default();
    (other_roots, agent_roots)
}

fn root_contains_path(root: &str, full_path: &str) -> bool {
    let trimmed = root.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return full_path.starts_with('/');
    }
    let Some(rest) = full_path.strip_prefix(trimmed) else {
        return false;
    };
    rest.is_empty() || rest.starts_with(['/', '\\'])
}

async fn search_workspace_candidates(
    client: &t3_client::supervisor::ClientHandle,
    cwd: &str,
    query: &str,
) -> Vec<WorkspaceEntryCandidate> {
    let payload = json!({
        "cwd": cwd,
        "query": query,
        "limit": WORKSPACE_BASENAME_LOOKUP_LIMIT,
        "kind": "file",
    });
    let Ok(reply) = client.request(SEARCH_ENTRIES, payload).await else {
        return Vec::new();
    };
    let Ok(listing) = decode_search(&reply) else {
        return Vec::new();
    };
    listing
        .entries
        .into_iter()
        .map(|entry| WorkspaceEntryCandidate {
            path: entry.path,
            kind: match entry.kind {
                t3_core::files::EntryKind::File => WorkspaceEntryKind::File,
                t3_core::files::EntryKind::Directory => WorkspaceEntryKind::Directory,
            },
        })
        .collect()
}

async fn filesystem_metadata_entries(
    client: &t3_client::supervisor::ClientHandle,
    paths: &[String],
) -> Option<Vec<Option<String>>> {
    if paths.is_empty() {
        return Some(Vec::new());
    }
    let reply = client.request("filesystem.getMetadata", json!({ "paths": paths })).await.ok()?;
    let entries = reply.get("entries")?.as_array()?;
    Some(
        entries
            .iter()
            .map(|entry| entry.get("kind").and_then(Value::as_str).map(str::to_owned))
            .collect(),
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_and_open_workspace_file_link(
    fallback_file: ResolvedFileLink,
    lookup_rel: String,
    cwd: String,
    query: String,
    ctx: MarkdownActionContext,
    source_key: Option<String>,
    click: LinkClickAction,
    cx: &mut App,
) {
    let Some(client) = ctx.state.read(cx).client_handle() else {
        open_resolved_file_now(&fallback_file, None, &click, &ctx, cx);
        return;
    };
    let (other_roots, agent_roots) = {
        let app = ctx.state.read(cx);
        let target_thread_id = resolve_action_target(&ctx, app).0;
        candidate_project_and_agent_roots(app, ctx.thread_view.as_ref(), target_thread_id.as_deref(), source_key.as_deref(), &cwd, cx)
    };
    let runtime = ctx.state.read(cx).runtime().clone();

    cx.spawn(async move |cx| {
        let fallback_full = fallback_file.full_path.clone();
        let resolved: Option<(String, Option<String>)> = runtime
            .spawn(async move {
                let is_basename = needs_workspace_basename_lookup(&lookup_rel);
                if !is_basename {
                    let literal_path = resolve_full_and_relative(&lookup_rel, Some(&cwd)).0;
                    if let Some(entries) = filesystem_metadata_entries(&client, &[literal_path]).await
                        && entries.first().is_some_and(Option::is_some)
                    {
                        return None;
                    }
                }

                let active_entries = search_workspace_candidates(&client, &cwd, &query).await;
                if let Some(matched) = pick_workspace_link_match(&lookup_rel, &cwd, &active_entries) {
                    let (full, _) = resolve_full_and_relative(&matched, Some(&cwd));
                    return Some((full, Some(cwd)));
                }

                if !other_roots.is_empty() || !agent_roots.is_empty() {
                    let cwd_exists = if is_basename {
                        let literal_path = resolve_full_and_relative(&lookup_rel, Some(&cwd)).0;
                        filesystem_metadata_entries(&client, &[literal_path])
                            .await
                            .is_some_and(|entries| entries.first().is_some_and(Option::is_some))
                    } else {
                        false
                    };
                    if cwd_exists {
                        return Some((fallback_full, Some(cwd)));
                    }

                    if !other_roots.is_empty() {
                        let mut project_searches = Vec::with_capacity(other_roots.len());
                        for root in &other_roots {
                            let candidates = search_workspace_candidates(&client, root, &query).await;
                            project_searches.push((root.as_str(), candidates));
                        }
                        let mut borrowed: Vec<(&str, &[WorkspaceEntryCandidate])> = Vec::with_capacity(project_searches.len() + 1);
                        borrowed.push((cwd.as_str(), active_entries.as_slice()));
                        borrowed.extend(
                            project_searches
                                .iter()
                                .map(|(root, entries)| (*root, entries.as_slice())),
                        );
                        if let Some(matched_full) = pick_project_link_match(&lookup_rel, borrowed) {
                            let matched_root = root_contains_path(&cwd, &matched_full)
                                .then(|| cwd.clone())
                                .or_else(|| {
                                    other_roots
                                        .iter()
                                        .find(|root| root_contains_path(root, &matched_full))
                                        .cloned()
                                });
                            return Some((matched_full, matched_root));
                        }
                    }

                    if !agent_roots.is_empty() {
                        let agent_paths: Vec<String> = agent_roots
                            .iter()
                            .map(|root| resolve_full_and_relative(&lookup_rel, Some(root)).0)
                            .collect();
                        if let Some(kinds) = filesystem_metadata_entries(&client, &agent_paths).await {
                            for (index, kind) in kinds.iter().enumerate() {
                                if kind.as_deref() == Some("file") {
                                    return Some((agent_paths[index].clone(), Some(agent_roots[index].clone())));
                                }
                            }
                        }
                        if is_basename {
                            for root in &agent_roots {
                                let entries = search_workspace_candidates(&client, root, &query).await;
                                if let Some(matched) = pick_workspace_link_match(&lookup_rel, root, &entries) {
                                    let (full, _) = resolve_full_and_relative(&matched, Some(root));
                                    return Some((full, Some(root.clone())));
                                }
                            }
                        }
                    }
                }
                None
            })
            .await
            .ok()
            .flatten();

        cx.update(|cx| {
            let (mut target_file, resolved_root) = match resolved {
                Some((full_path, root)) => {
                    let mut next = fallback_file.clone();
                    next.relative_path = Some(t3_core::files::file_surface_path(&full_path, root.as_deref()).unwrap_or_else(|| full_path.clone()));
                    next.full_path = full_path;
                    (next, root)
                }
                None => (fallback_file, None),
            };
            if resolved_root.is_some() && target_file.relative_path.is_none() {
                target_file.relative_path = Some(target_file.full_path.clone());
            }
            open_resolved_file_now(&target_file, resolved_root.as_deref(), &click, &ctx, cx);
        });
    })
    .detach();
}

fn open_in_editor_with_root(app: &mut AppState, cwd: &str, relative_or_full: &str, line: Option<u64>, cx: &mut gpui::Context<AppState>) -> bool {
    let available = app
        .server_config
        .config
        .as_ref()
        .map(|config| config.available_editors.clone())
        .unwrap_or_default();
    let Some((editor, label)) = preferred_editor(&available, None) else {
        return false;
    };
    let target = editor_target(cwd, relative_or_full, line);
    let request = app.rpc(OPEN_IN_EDITOR, open_in_editor_request_with_root(&target, editor, Some(cwd)));
    cx.spawn(async move |this, cx| {
        let result = request.await;
        let _ = this.update(cx, |state, cx| match result {
            Ok(_) => state.announce(format!("Opened in {label}"), None, cx),
            Err(error) => state.show_toast(
                Toast::new(ToastKind::Error, format!("Could not open in {label}")).description(error.to_string()),
                cx,
            ),
        });
    })
    .detach();
    true
}

fn reveal_or_open_in_editor(
    ctx: &MarkdownActionContext,
    file: &ResolvedFileLink,
    resolved_root: Option<&str>,
    reveal: bool,
    cx: &mut App,
) {
    let state = &ctx.state;
    let cwd = resolved_root.map(str::to_owned).unwrap_or_else(|| {
        let app = state.read(cx);
        resolve_action_target(ctx, app).1.unwrap_or_else(|| "/".to_owned())
    });
    let path_for_editor = file.relative_path.as_deref().unwrap_or(&file.full_path);

    if reveal {
        let can_reveal = state
            .read(cx)
            .server_config
            .config
            .as_ref()
            .is_some_and(|cfg| cfg.available_editors.iter().any(|e| e == "file-manager"));
        if can_reveal {
            let payload = open_in_editor_request_with_root(&file.full_path, "file-manager", None);
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

    let opened = state.update(cx, |app, cx| open_in_editor_with_root(app, &cwd, path_for_editor, file.line, cx));
    if !opened && file.full_path.starts_with('/') {
        cx.open_url(&file_url(&file.full_path));
    }
}

/// Attempts to open an inline issue URL in the right panel (`openIssueLink.ts`,
/// `pingdotgg/t3code#38269`, `pingdotgg/t3code#38854`). Returns `true` when handled (either
/// synchronously or via an async standalone tracker lookup).
pub fn try_open_issue_link(
    url: &str,
    modifiers: gpui::Modifiers,
    action_ctx: Option<&MarkdownActionContext>,
    cx: &mut App,
) -> bool {
    if modifiers.platform || modifiers.control {
        return false;
    }
    let Some(ctx) = action_ctx else {
        return false;
    };
    let (env_id, preferred_project, projects, supported) = {
        let app = ctx.state.read(cx);
        let env_id = app
            .environments
            .active
            .as_deref()
            .or_else(|| app.environment())
            .map(str::to_owned);
        let target_thread = resolve_action_target(ctx, app).0;
        let preferred_project = target_thread
            .as_deref()
            .and_then(|id| app.thread_shell(id))
            .map(|t| t.project_id.clone())
            .or_else(|| app.draft_project.clone());
        let projects = crate::issues::issue_link_projects_for(app, env_id.as_deref());
        let supported = crate::issues::issues_supported_for(app, env_id.as_deref(), cx);
        (env_id, preferred_project, projects, supported)
    };
    if !supported || projects.is_empty() {
        return false;
    }
    let trackers = env_id
        .as_deref()
        .and_then(|id| crate::issues::cached_issue_trackers(id, cx));
    if let Some(resolved) = t3_core::issues::links::resolve_issue_link_target(
        url,
        &projects,
        preferred_project.as_deref(),
        trackers.as_ref(),
    ) {
        open_resolved_issue_link(&ctx.state, resolved, cx);
        return true;
    }
    if trackers.is_none() && t3_core::issues::links::is_standalone_issue_link(url) {
        let state = ctx.state.clone();
        let url_owned = url.to_owned();
        let read_task = crate::issues::read_issue_trackers(state.read(cx), env_id.as_deref());
        if read_task.is_some() {
            cx.spawn(async move |cx| {
                let result = crate::issues::settle(read_task).await;
                cx.update(|cx| {
                    if let Ok(list) = result {
                        if let Some(id) = env_id.clone() {
                            crate::issues::cache_issue_trackers(id, list.clone(), cx);
                        }
                        if let Some(resolved) = t3_core::issues::links::resolve_issue_link_target(
                            &url_owned,
                            &projects,
                            preferred_project.as_deref(),
                            Some(&list),
                        ) {
                            open_resolved_issue_link(&state, resolved, cx);
                            return;
                        }
                    }
                    cx.open_url(&url_owned);
                });
            })
            .detach();
            return true;
        }
    }
    false
}

fn open_resolved_issue_link(
    state: &Entity<AppState>,
    resolved: t3_core::issues::links::ResolvedIssueLink,
    cx: &mut App,
) {
    let target = t3_core::right_panel::IssueTarget::from_ref(
        Some(resolved.environment_id),
        &resolved.reference,
        None,
        Some(resolved.url),
    );
    state.update(cx, |app, cx| {
        if app.issues_open {
            app.open_issue_surface_for_key(
                t3_core::right_panel::ISSUES_PANEL_THREAD_KEY,
                target,
                cx,
            );
        } else {
            app.open_issue_surface(target, cx);
        }
    });
}

fn is_http_or_https(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || trimmed
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}

pub(crate) fn open_browser_link(
    url: &str,
    modifiers: gpui::Modifiers,
    state: Option<&Entity<AppState>>,
    thread_id: Option<String>,
    cx: &mut App,
) {
    let browser_link_target = crate::settings::client_settings(cx).browser_link_target();
    let active_thread_id = thread_id.or_else(|| {
        state.and_then(|s| {
            let app = s.read(cx);
            (!app.pull_requests_open && !app.issues_open)
                .then(|| app.selected_thread.clone())
                .flatten()
        })
    });
    if !modifiers.platform
        && !modifiers.control
        && is_http_or_https(url)
        && browser_link_target == BrowserLinkTarget::App
        && let Some(previews) = crate::preview::previews(cx)
        && let Some(thread_id) = active_thread_id
    {
        previews.update(cx, |p, cx| p.open_link(thread_id, url.to_owned(), cx));
        return;
    }

    cx.open_url(url);
}

fn open_external_link(
    url: &str,
    modifiers: gpui::Modifiers,
    action_ctx: Option<&MarkdownActionContext>,
    cx: &mut App,
) {
    let pr_links_in_app = crate::settings::client_settings(cx).pull_request_links_open_in_app();
    if !modifiers.platform
        && !modifiers.control
        && pr_links_in_app
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

    if try_open_issue_link(url, modifiers, action_ctx, cx) {
        return;
    }

    open_browser_link(
        url,
        modifiers,
        action_ctx.map(|c| &c.state),
        action_ctx.and_then(|c| c.thread_id.clone()),
        cx,
    );
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
            if let Some(parent) = app
                .thread_shell(&citation.thread_id)
                .filter(|thread| thread.lineage.side_chat == Some(true))
                .and_then(|thread| thread.lineage.parent_thread_id.clone())
            {
                if app.selected_thread.as_deref() != Some(parent.as_str()) {
                    app.select_thread(&parent, cx);
                }
                app.show_side_chat_tab(&parent, &citation.thread_id, cx);
            } else if app.selected_thread.as_deref() != Some(citation.thread_id.as_str()) {
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

    #[gpui::test]
    fn open_thread_link_reports_unreachable_remote_environment_without_active_fallback(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });

        let target = ThreadLinkTarget {
            version: 1,
            environment_id: Some("env-offline".into()),
            thread_id: "thr-remote".into(),
            allow_cross_env_fallback: false,
        };
        let notices = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let notices_sub = notices.clone();
        let _sub = cx.update(|cx| {
            cx.subscribe(&state, move |_, ev, _| {
                if matches!(ev, AppEvent::Notice) {
                    notices_sub.set(notices_sub.get() + 1);
                }
            })
        });
        cx.update(|cx| {
            open_thread_link(&target, &state, cx);
            let app = state.read(cx);
            assert!(app.selected_thread.is_none());
            assert!(!app.has_pending_scoped_route());
        });
        assert!(notices.get() >= 1, "expected toast Notice event for unreachable remote env");
    }

    #[gpui::test]
    fn model_mention_popover_opens_with_resolved_provider_metadata(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });
        let cfg: t3_core::server_config::ServerConfig = serde_json::from_value(serde_json::json!({
            "providers": [{
                "instanceId": "claude",
                "driver": "claudeAgent",
                "displayName": "Claude Code",
                "enabled": true,
                "installed": true,
                "status": "ready",
                "models": [{
                    "slug": "claude-sonnet-4",
                    "name": "Claude Sonnet 4",
                    "shortName": "Sonnet 4",
                    "subProvider": "Anthropic"
                }]
            }]
        }))
        .expect("valid ServerConfig");
        state.update(cx, |app, _| {
            app.server_config.config = Some(cfg);
        });

        let window = cx.add_window(|_, _| gpui::Empty);
        window
            .update(cx, |_, window, cx| {
                let owner = state.entity_id();
                open_model_mention_popover(
                    "claude",
                    "claude-sonnet-4",
                    gpui::point(px(24.), px(48.)),
                    &state,
                    owner,
                    window,
                    cx,
                );
                let active = cx
                    .try_global::<ModelMentionPopoverGlobal>()
                    .and_then(|g| g.active.as_ref())
                    .expect("popover should be active");
                assert_eq!(active.label, "Sonnet 4");
                assert_eq!(active.provider_label, "Anthropic");
                assert_eq!(active.source, "model:claude/claude-sonnet-4");
                close_model_mention_popover(cx);
                assert!(
                    cx.try_global::<ModelMentionPopoverGlobal>()
                        .and_then(|g| g.active.as_ref())
                        .is_none()
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn candidate_project_roots_keeps_all_other_projects_and_excludes_thread_project(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });
        state.update(cx, |app, _| {
            let projects = [
                ("p-thread", "/tmp/worktrees/main"),
                ("p-same-proj", "/tmp/worktrees/other-in-same-project"),
                ("p-1", "/tmp/other-1"),
                ("p-2", "/tmp/other-2"),
                ("p-3", "/tmp/other-3"),
                ("p-4", "/tmp/other-4"),
                ("p-5", "/tmp/other-5"),
                ("p-dup", "/tmp/other-5"),
            ]
            .into_iter()
            .map(|(id, path)| {
                serde_json::from_value(serde_json::json!({
                    "id": if id == "p-same-proj" { "p-thread" } else { id },
                    "title": id,
                    "workspaceRoot": path,
                    "createdAt": "2026-01-01T00:00:00Z",
                    "updatedAt": "2026-01-01T00:00:00Z"
                }))
                .unwrap()
            })
            .collect();
            let threads = vec![serde_json::from_value(serde_json::json!({
                "id": "t-1",
                "projectId": "p-thread",
                "title": "t-1",
                "createdAt": "2026-01-01T00:00:00Z",
                "updatedAt": "2026-01-01T00:00:00Z",
                "lastVisitedAt": null,
                "providerInstanceId": "replay",
                "modelSelection": {"instanceId": "replay", "model": "replay"},
                "runtimeMode": "auto",
                "interactionMode": "default",
                "worktreePath": "/tmp/worktrees/main",
                "lineage": {"rootThreadId": "t-1"},
                "status": "idle",
                "hasActionableProposedPlan": false,
                "itemCount": 0,
                "visibleItemCount": 0
            }))
            .unwrap()];
            app.shell.snapshot = Some(ShellSnapshot {
                projects,
                threads,
                ..Default::default()
            });
        });

        cx.update(|cx| {
            let app = state.read(cx);
            let (other_roots, _) = candidate_project_and_agent_roots(app, None, Some("t-1"), None, "/tmp/worktrees/main", cx);
            assert_eq!(
                other_roots,
                vec![
                    "/tmp/other-1".to_owned(),
                    "/tmp/other-2".to_owned(),
                    "/tmp/other-3".to_owned(),
                    "/tmp/other-4".to_owned(),
                    "/tmp/other-5".to_owned(),
                ]
            );
            assert!(root_contains_path("/tmp/repo", "/tmp/repo/src/lib.rs"));
            assert!(root_contains_path("/tmp/repo/", "/tmp/repo/src/lib.rs"));
            assert!(!root_contains_path("/tmp/repo", "/tmp/repo-other/src/lib.rs"));
        });
    }

    #[cfg(feature = "snapshot")]
    #[gpui::test]
    fn macos_app_bundle_policy_requires_darwin_local_client_host_operate_and_file_manager(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let _guard = runtime.enter();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            cx.new(|cx| AppState::new(runtime.handle().clone(), cx))
        });

        let (op_client, _op_fixture) = t3_client::supervisor::ClientFixture::handle_with_http_base(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": ["orchestration:read", "orchestration:operate"]
            }))
            .unwrap(),
            "http://127.0.0.1:1".into(),
        );
        let (ro_client, _ro_fixture) = t3_client::supervisor::ClientFixture::handle_with_http_base(
            serde_json::from_value(serde_json::json!({
                "authenticated": true,
                "permissions": ["orchestration:read"]
            }))
            .unwrap(),
            "http://127.0.0.1:1".into(),
        );

        let darwin_cfg = |editors: Value, os: &str| -> t3_core::server_config::ServerConfig {
            serde_json::from_value(serde_json::json!({
                "environment": {
                    "environmentId": "env-1",
                    "label": "Mac",
                    "platform": { "machine": "arm64", "os": os }
                },
                "availableEditors": editors
            }))
            .unwrap()
        };
        let saved_env = |http_base: &str, local: bool, ssh: bool| t3_client::store::SavedEnvironment {
            id: "saved-1".into(),
            environment_id: Some("env-1".into()),
            label: "Mac".into(),
            disabled: false,
            http_base: http_base.into(),
            access_token: Some("tok".into()),
            server_version: None,
            local,
            ssh: ssh.then(|| serde_json::from_value(serde_json::json!({
                "alias": "remote",
                "hostname": "remote.example"
            })).unwrap()),
        };

        state.update(cx, |app, cx| {
            app.environments.active = Some("saved-1".into());
            app.environments.environments = vec![saved_env("http://127.0.0.1:3773", true, false)];
            let mut cfg_state = app.server_config.clone();
            cfg_state.config = Some(darwin_cfg(serde_json::json!(["file-manager", "cursor"]), "darwin"));
            app.install_active_environment(op_client.clone(), app.shell.clone(), cfg_state, t3_client::supervisor::ConnectionStatus::Connected, cx);
            assert_eq!(macos_app_bundle_policy(app, "/Applications/Calculator.app"), MacosAppBundlePolicy::OpenViaFileManager);
            assert_eq!(macos_app_bundle_policy(app, "/tmp/README.md"), MacosAppBundlePolicy::NotAppBundle);

            // Missing file-manager -> Blocked
            app.server_config.config = Some(darwin_cfg(serde_json::json!(["cursor"]), "darwin"));
            assert_eq!(macos_app_bundle_policy(app, "/Applications/Calculator.app"), MacosAppBundlePolicy::Blocked);

            // Remote host -> Blocked
            app.server_config.config = Some(darwin_cfg(serde_json::json!(["file-manager"]), "darwin"));
            app.environments.environments = vec![saved_env("https://remote.example", false, false)];
            assert_eq!(macos_app_bundle_policy(app, "/Applications/Calculator.app"), MacosAppBundlePolicy::Blocked);

            // SSH tunnel -> Blocked even on loopback http_base
            app.environments.environments = vec![saved_env("http://127.0.0.1:3999", false, true)];
            assert_eq!(macos_app_bundle_policy(app, "/Applications/Calculator.app"), MacosAppBundlePolicy::Blocked);

            // Non-darwin server -> NotAppBundle
            app.environments.environments = vec![saved_env("http://127.0.0.1:3773", true, false)];
            app.server_config.config = Some(darwin_cfg(serde_json::json!(["file-manager"]), "linux"));
            assert_eq!(macos_app_bundle_policy(app, "/Applications/Calculator.app"), MacosAppBundlePolicy::NotAppBundle);

            // Missing orchestration:operate -> Blocked
            app.server_config.config = Some(darwin_cfg(serde_json::json!(["file-manager"]), "darwin"));
            app.install_active_environment(ro_client.clone(), app.shell.clone(), app.server_config.clone(), t3_client::supervisor::ConnectionStatus::Connected, cx);
            assert_eq!(macos_app_bundle_policy(app, "/Applications/Calculator.app"), MacosAppBundlePolicy::Blocked);
        });
    }

    #[gpui::test]
    fn inline_issue_links_open_right_panel_issue_surface_and_respect_modifiers(cx: &mut gpui::TestAppContext) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let state = cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
            crate::settings::store::init(cx);
            let state = cx.new(|cx| AppState::new(rt.handle().clone(), cx));
            state.update(cx, |app, _| {
                app.environments.active = Some("env-1".into());
                app.selected_thread = Some("thr-1".into());
                app.server_config.config = Some(
                    serde_json::from_value(serde_json::json!({
                        "environment": { "environmentId": "env-1", "label": "Mac" },
                        "availableEditors": []
                    }))
                    .unwrap(),
                );
                app.shell.snapshot = Some(
                    serde_json::from_value(serde_json::json!({
                        "snapshotSequence": 1,
                        "projects": [{
                            "id": "p1",
                            "title": "Web",
                            "workspaceRoot": "/tmp/web",
                            "repositoryIdentity": {
                                "canonicalKey": "github.com/acme/web",
                                "provider": "github",
                                "displayName": "acme/web"
                            },
                            "defaultModelSelection": null,
                            "scripts": [],
                            "createdAt": "2026-01-01T00:00:00Z",
                            "updatedAt": "2026-01-01T00:00:00Z"
                        }],
                        "threads": [],
                        "updatedAt": "2026-01-01T00:00:00Z"
                    }))
                    .unwrap(),
                );
            });
            crate::issues::cache_issue_trackers(
                "env-1".into(),
                serde_json::from_value(serde_json::json!({
                    "trackers": [{
                        "tracker": "linear",
                        "displayName": "Linear",
                        "host": "linear.app",
                        "scopeNoun": "team",
                        "credential": { "label": "Personal API key" },
                        "connected": true,
                        "account": "alice",
                        "workspace": "acme",
                        "scopes": []
                    }],
                    "bindings": { "p1": { "tracker": "linear", "scope": "WEB" } }
                }))
                .unwrap(),
                cx,
            );
            state
        });

        let ctx = MarkdownActionContext {
            state: state.clone(),
            owner: state.entity_id(),
            cwd: Some("/tmp/web".into()),
            thread_id: Some("thr-1".into()),
            thread_view: None,
        };

        cx.update(|cx| {
            // Cmd-click bypasses in-app issue opening.
            assert!(!try_open_issue_link(
                "https://github.com/acme/web/issues/42",
                gpui::Modifiers { platform: true, ..Default::default() },
                Some(&ctx),
                cx,
            ));
            // Plain click on GitHub issue opens SurfaceKind::Issue in active thread's right panel.
            assert!(try_open_issue_link(
                "https://github.com/acme/web/issues/42",
                gpui::Modifiers::default(),
                Some(&ctx),
                cx,
            ));
            let active = state.read(cx).panels.panels.active_surface("thr-1").unwrap().clone();
            assert_eq!(active.title(), "#42");
            assert!(matches!(
                active.kind,
                t3_core::right_panel::SurfaceKind::Issue(ref target) if target.key == "42"
            ));

            // Plain click on Linear issue opens Linear SurfaceKind::Issue (`#38854`).
            assert!(try_open_issue_link(
                "https://linear.app/acme/issue/WEB-17/fix-it",
                gpui::Modifiers::default(),
                Some(&ctx),
                cx,
            ));
            let linear = state.read(cx).panels.panels.active_surface("thr-1").unwrap().clone();
            assert_eq!(linear.title(), "WEB-17");

            // Foreign Linear workspace stays an ordinary link.
            assert!(!try_open_issue_link(
                "https://linear.app/other/issue/WEB-17",
                gpui::Modifiers::default(),
                Some(&ctx),
                cx,
            ));
        });
    }
}
