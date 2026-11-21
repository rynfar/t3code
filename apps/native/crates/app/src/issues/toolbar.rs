//! Issues page toolbar (`IssuesPage.tsx`, `IssueListFilterPickers.tsx`, `SavedViewsMenu.tsx`):
//! search input, sort dropdown, filters dropdown (state, involvement, project, labels, assignee),
//! saved views menu + save/rename modal, tracker/host switcher, and manual refresh.

use std::collections::BTreeSet;

use gpui::{
    AnyElement, App, Context, FontWeight, IntoElement, ParentElement, Styled, WeakEntity, Window,
    div, prelude::*, px,
};
use gpui_component::input::Input;
use gpui_component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::issues::list::{IssueListPreferences, MAX_LABELS};
use t3_core::issues::saved_views::{
    is_view_modified, issue_view_matches, issue_view_suggested_name, resolve_selected_view,
    saved_view_name_error,
};
use t3_core::issues::wire::{IssueInvolvement, IssueListSort, IssueListState};

use super::page::{IssuesPage, SavedViewEditor};
use crate::components::motion::Spinner;
use crate::i18n::t;
use crate::icons::Icon;
use crate::pull_requests::controls::{Button, Size, Variant};
use crate::theme::{hsla, palette};

const VISIBLE_CHOICES: usize = 30;

/// Places active selections ahead of alphabetical choices before truncating to `limit` so a
/// chosen label that sorts past `limit` always stays visible and clearable
/// (`IssueListFilterPickers.tsx:182-188`).
pub(super) fn prioritize_labels(
    active: &[String],
    known: impl IntoIterator<Item = String>,
    limit: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in active {
        if !out.iter().any(|existing| existing.eq_ignore_ascii_case(name)) {
            out.push(name.clone());
        }
    }
    let sorted: BTreeSet<String> = known.into_iter().collect();
    for name in sorted {
        if !out.iter().any(|existing| existing.eq_ignore_ascii_case(&name)) {
            out.push(name);
        }
    }
    out.truncate(limit);
    out
}

/// Places the active assignee ahead of alphabetical choices before truncating to `limit` so a
/// chosen person who sorts past `limit` always stays visible and clearable
/// (`IssueListFilterPickers.tsx:248-253`).
pub(super) fn prioritize_assignees(
    active: Option<&str>,
    known: impl IntoIterator<Item = String>,
    limit: usize,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(active) = active.map(str::trim).filter(|a| !a.is_empty()) {
        out.push(active.to_owned());
    }
    let sorted: BTreeSet<String> = known.into_iter().collect();
    for login in sorted {
        if !out.iter().any(|existing| existing.eq_ignore_ascii_case(&login)) {
            out.push(login);
        }
    }
    out.truncate(limit);
    out
}

pub fn render(
    page: &mut IssuesPage,
    _window: &mut Window,
    cx: &mut Context<IssuesPage>,
) -> impl IntoElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let typed_trimmed = page.typed.trim();
    let busy = (!typed_trimmed.is_empty() && typed_trimmed != page.sent.trim())
        || (!page.sent.trim().is_empty() && page.is_pending());
    let entity = cx.entity().downgrade();

    let sort = page.prefs.effective_sort();
    let sort_entity = entity.clone();
    let sort_menu = Button::new("issues-sort", Variant::Outline, Size::Default)
        .icon(Icon::ArrowDownUp)
        .label(t(cx, "Sort").to_owned())
        .tooltip(format!("{}: {}", t(cx, "Sort issues"), t(cx, sort.label())))
        .dropdown_menu(move |menu, _, cx| {
            IssueListSort::ALL.iter().fold(menu, |menu, option| {
                let option = *option;
                let entity = sort_entity.clone();
                menu.item(
                    PopupMenuItem::new(t(cx, option.label()).to_owned())
                        .checked(option == sort)
                        .on_click(move |_, _, cx| {
                            update(&entity, cx, move |prefs| {
                                prefs.order = (option == IssueListSort::Created).then_some(option);
                            });
                        }),
                )
            })
        });

    let filters = filters_menu(page, entity.clone(), cx);
    let saved_views = saved_views_menu(page, entity.clone(), cx);
    let host_menu = host_filter_menu(page, entity.clone(), cx);
    let pending = page.is_pending();
    let refresh = Button::new("issues-refresh", Variant::Outline, Size::Icon)
        .icon(Icon::RefreshCw)
        .busy(pending)
        .disabled(pending)
        .tooltip(t(cx, "Refresh issues").to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.manual_refresh(cx)));

    h_flex()
        .flex_none()
        .flex_wrap()
        .gap(px(8.))
        .px(px(20.))
        .pt(px(16.))
        .pb(px(12.))
        .child(
            h_flex()
                .flex_1()
                .min_w(px(200.))
                .h(px(32.))
                .px(px(10.))
                .gap(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(p.input))
                .bg(if dark {
                    hsla(p.input).opacity(0.32)
                } else {
                    hsla(p.background)
                })
                .child(if busy {
                    Spinner::new("issues-search-spin", px(16.), hsla(p.muted_foreground)).into_any_element()
                } else {
                    Icon::Search.sized(px(16.)).text_color(hsla(p.muted_foreground)).into_any_element()
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&page.search).appearance(false).bordered(false)),
                )
                .when(!page.typed.is_empty(), |this| {
                    this.child(
                        Button::new("issues-clear-search", Variant::GhostMuted, Size::Xs)
                            .icon(Icon::X)
                            .tooltip(t(cx, "Clear search").to_owned())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.clear_query(window, cx);
                            })),
                    )
                }),
        )
        .child(sort_menu)
        .child(filters)
        .child(saved_views)
        .children(host_menu)
        .child(refresh)
}

fn update(
    entity: &WeakEntity<IssuesPage>,
    cx: &mut App,
    change: impl FnOnce(&mut IssueListPreferences) + 'static,
) {
    if let Some(entity) = entity.upgrade() {
        entity.update(cx, |page, cx| page.update_prefs(change, cx));
    }
}

struct Radio<T> {
    label: &'static str,
    options: Vec<(T, String)>,
    current: T,
    set: fn(&mut IssueListPreferences, T),
}

fn radio<T: Copy + PartialEq + 'static>(
    menu: PopupMenu,
    spec: Radio<T>,
    entity: &WeakEntity<IssuesPage>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Radio { label, options, current, set } = spec;
    let shown = options
        .iter()
        .find(|(value, _)| *value == current)
        .map_or_else(String::new, |(_, text)| text.clone());
    let entity = entity.clone();
    menu.submenu(format!("{} · {shown}", t(cx, label)), window, cx, move |menu, _, _| {
        options.iter().fold(menu, |menu, (value, text)| {
            let value = *value;
            let entity = entity.clone();
            menu.item(
                PopupMenuItem::new(text.clone())
                    .checked(value == current)
                    .on_click(move |_, _, cx| {
                        update(&entity, cx, move |prefs| set(prefs, value));
                    }),
            )
        })
    })
}

fn filters_menu(
    page: &IssuesPage,
    entity: WeakEntity<IssuesPage>,
    cx: &mut Context<IssuesPage>,
) -> AnyElement {
    let prefs = page.prefs.clone();
    let filter_projects = page.filter_projects(cx);
    let scoped_project = page.canonical_scoped_project(cx);
    let candidates = page.candidates_tracker.merged();
    let failed_candidates = !page.candidates_tracker.failed_targets().is_empty();

    // Active selections lead so choices beyond VISIBLE_CHOICES can still be seen and cleared.
    let visible_labels = prioritize_labels(
        &prefs.label,
        candidates
            .labels
            .iter()
            .map(|label| label.name.clone())
            .chain(
                page.groups
                    .iter()
                    .flat_map(|group| group.rows.iter())
                    .flat_map(|row| row.entry.labels.iter().map(|label| label.name.clone())),
            ),
        VISIBLE_CHOICES,
    );

    let visible_assignees = prioritize_assignees(
        prefs.assignee.as_deref(),
        candidates
            .assignees
            .iter()
            .map(|actor| actor.login.clone())
            .chain(
                page.groups
                    .iter()
                    .flat_map(|group| group.rows.iter())
                    .flat_map(|row| row.entry.assignees.iter().map(|actor| actor.login.clone())),
            ),
        VISIBLE_CHOICES,
    );
    let candidates_pending = candidates.is_pending;
    let candidates_error = candidates.error.clone();
    let candidates_truncated = candidates.truncated;
    let filter_count = prefs.filter_count();

    let state_options: Vec<(IssueListState, String)> = IssueListState::ALL
        .iter()
        .map(|s| (*s, t(cx, s.label()).to_owned()))
        .collect();
    let involvement_options: Vec<(IssueInvolvement, String)> = IssueInvolvement::ALL
        .iter()
        .map(|i| (*i, t(cx, i.label()).to_owned()))
        .collect();

    Button::new("issues-filters", Variant::Outline, Size::Default)
        .icon(Icon::ListFilter)
        .label(t(cx, "Filters").to_owned())
        .count(filter_count)
        .on_click(cx.listener(|this, _, _, cx| this.ensure_candidates(cx)))
        .dropdown_menu(move |menu, window, cx| {
            let menu = radio(
                menu,
                Radio {
                    label: "State",
                    options: state_options.clone(),
                    current: prefs.status,
                    set: |p, v| p.status = v,
                },
                &entity,
                window,
                cx,
            );
            let mut menu = radio(
                menu,
                Radio {
                    label: "Involvement",
                    options: involvement_options.clone(),
                    current: prefs.who,
                    set: |p, v| p.who = v,
                },
                &entity,
                window,
                cx,
            );

            if filter_projects.len() > 1 || scoped_project.is_some() {
                let shown_project = scoped_project
                    .as_ref()
                    .map(|scoped| {
                        filter_projects
                            .iter()
                            .find(|p| {
                                p.environment_id == scoped.environment_id
                                    && p.project_id == scoped.project_id
                            })
                            .map(|p| p.title.clone())
                            .unwrap_or_else(|| t(cx, "Unavailable project").to_owned())
                    })
                    .unwrap_or_else(|| t(cx, "All projects").to_owned());
                let project_entity = entity.clone();
                let projects = filter_projects.clone();
                let current_project = scoped_project.clone();
                menu = menu.submenu(
                    format!("{} · {shown_project}", t(cx, "Project")),
                    window,
                    cx,
                    move |menu, _, cx| {
                        let clear_entity = project_entity.clone();
                        let mut menu = menu.item(
                            PopupMenuItem::new(t(cx, "All projects").to_owned())
                                .checked(current_project.is_none())
                                .on_click(move |_, _, cx| {
                                    update(&clear_entity, cx, |prefs| {
                                        prefs.issue_server = None;
                                        prefs.issue_project = None;
                                    });
                                }),
                        );
                        if let Some(scoped) = &current_project
                            && !projects.iter().any(|p| {
                                p.environment_id == scoped.environment_id
                                    && p.project_id == scoped.project_id
                            })
                        {
                            menu = menu.item(
                                PopupMenuItem::new(t(cx, "Unavailable project").to_owned())
                                    .checked(true)
                                    .disabled(true),
                            );
                        }
                        projects.iter().fold(menu, |menu, project| {
                            let is_selected = current_project.as_ref().is_some_and(|s| {
                                s.environment_id == project.environment_id
                                    && s.project_id == project.project_id
                            });
                            let env_id = project.environment_id.clone();
                            let project_id = project.project_id.clone();
                            let entity = project_entity.clone();
                            menu.item(
                                PopupMenuItem::new(project.title.clone())
                                    .checked(is_selected)
                                    .on_click(move |_, _, cx| {
                                        let env_id = env_id.clone();
                                        let project_id = project_id.clone();
                                        update(&entity, cx, move |prefs| {
                                            prefs.issue_server = Some(env_id);
                                            prefs.issue_project = Some(project_id);
                                        });
                                    }),
                            )
                        })
                    },
                );
            }

            menu = menu.separator();

            // Labels submenu
            let selected_labels = prefs.label.clone();
            let labels_summary = if selected_labels.is_empty() {
                t(cx, "Any").to_owned()
            } else if selected_labels.len() == 1 {
                selected_labels[0].clone()
            } else {
                crate::i18n::t_fmt(
                    cx,
                    "{count, plural, one {{count} label} other {{count} labels}}",
                    &[("count", selected_labels.len().into())],
                )
            };
            let label_entity = entity.clone();
            let labels_list = visible_labels.clone();
            let labels_err = candidates_error.clone();
            menu = menu.submenu(
                format!("{} · {labels_summary}", t(cx, "Labels")),
                window,
                cx,
                move |mut menu, _, cx| {
                    if !selected_labels.is_empty() {
                        let clear_entity = label_entity.clone();
                        menu = menu
                            .item(
                                PopupMenuItem::new(crate::i18n::t_fmt(
                                    cx,
                                    "Clear {field}",
                                    &[("field", t(cx, "Labels").into())],
                                ))
                                .on_click(
                                    move |_, _, cx| {
                                        update(&clear_entity, cx, |prefs| prefs.label.clear());
                                    },
                                ),
                            )
                            .separator();
                    }
                    if labels_list.is_empty() && !candidates_pending {
                        let empty_msg = if labels_err.is_none() {
                            t(cx, "No labels found")
                        } else {
                            t(cx, "The labels could not be read")
                        };
                        menu = menu.item(PopupMenuItem::new(empty_msg.to_owned()).disabled(true));
                    }
                    for name in &labels_list {
                        let checked = selected_labels
                            .iter()
                            .any(|existing| existing.eq_ignore_ascii_case(name));
                        let name_owned = name.clone();
                        let entity = label_entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(name.clone())
                                .checked(checked)
                                .on_click(move |_, _, cx| {
                                    let name = name_owned.clone();
                                    update(&entity, cx, move |prefs| {
                                        if let Some(pos) = prefs
                                            .label
                                            .iter()
                                            .position(|l| l.eq_ignore_ascii_case(&name))
                                        {
                                            prefs.label.remove(pos);
                                        } else if prefs.label.len() < MAX_LABELS {
                                            prefs.label.push(name);
                                        }
                                    });
                                }),
                        );
                    }
                    if failed_candidates {
                        let retry_entity = label_entity.clone();
                        menu = menu.separator().item(
                            PopupMenuItem::new(t(cx, "Retry").to_owned())
                                .on_click(move |_, _, cx| {
                                    if let Some(page) = retry_entity.upgrade() {
                                        page.update(cx, |page, cx| {
                                            page.retry_failed_candidates(cx);
                                        });
                                    }
                                }),
                        );
                    }
                    if candidates_truncated {
                        menu = menu.item(
                            PopupMenuItem::new(
                                t(cx, "More labels exist than are listed here.").to_owned(),
                            )
                            .disabled(true),
                        );
                    }
                    menu
                },
            );

            // Assignee submenu
            let current_assignee = prefs.assignee.clone();
            let assignee_summary = current_assignee
                .clone()
                .unwrap_or_else(|| t(cx, "Anyone").to_owned());
            let assignee_entity = entity.clone();
            let assignees_list = visible_assignees.clone();
            let assignees_err = candidates_error.clone();
            menu.submenu(
                format!("{} · {assignee_summary}", t(cx, "Assignee")),
                window,
                cx,
                move |mut menu, _, cx| {
                    let clear_entity = assignee_entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(t(cx, "Anyone").to_owned())
                            .checked(current_assignee.is_none())
                            .on_click(move |_, _, cx| {
                                update(&clear_entity, cx, |prefs| prefs.assignee = None);
                            }),
                    );
                    for login in &assignees_list {
                        let checked = current_assignee
                            .as_deref()
                            .is_some_and(|a| a.eq_ignore_ascii_case(login));
                        let login_owned = login.clone();
                        let entity = assignee_entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(login.clone())
                                .checked(checked)
                                .on_click(move |_, _, cx| {
                                    let login = login_owned.clone();
                                    update(&entity, cx, move |prefs| prefs.assignee = Some(login));
                                }),
                        );
                    }
                    if assignees_list.is_empty() && !candidates_pending && assignees_err.is_some() {
                        menu = menu.item(
                            PopupMenuItem::new(t(cx, "The people could not be read").to_owned())
                                .disabled(true),
                        );
                    }
                    if failed_candidates {
                        let retry_entity = assignee_entity.clone();
                        menu = menu.separator().item(
                            PopupMenuItem::new(t(cx, "Retry").to_owned())
                                .on_click(move |_, _, cx| {
                                    if let Some(page) = retry_entity.upgrade() {
                                        page.update(cx, |page, cx| {
                                            page.retry_failed_candidates(cx);
                                        });
                                    }
                                }),
                        );
                    }
                    if candidates_truncated {
                        menu = menu.item(
                            PopupMenuItem::new(
                                t(cx, "More people exist than are listed here.").to_owned(),
                            )
                            .disabled(true),
                        );
                    }
                    menu
                },
            )
        })
        .into_any_element()
}

fn saved_views_menu(
    page: &IssuesPage,
    entity: WeakEntity<IssuesPage>,
    cx: &App,
) -> impl IntoElement {
    let views = page.saved_views.clone();
    let prefs = page.prefs.clone();
    let selected = resolve_selected_view(
        &views,
        page.selected_view_id.as_deref(),
        &prefs,
        issue_view_matches,
    )
    .cloned();
    let modified = is_view_modified(selected.as_ref(), &prefs, issue_view_matches);
    let trigger_label = match &selected {
        Some(view) if modified => format!("{}*", view.name),
        Some(view) => view.name.clone(),
        None => t(cx, "Views").to_owned(),
    };
    let tooltip_text = selected
        .as_ref()
        .map(|view| view.name.clone())
        .unwrap_or_else(|| {
            crate::i18n::t_fmt(
                cx,
                "{list, select, pullRequest {Saved pull request views} other {Saved issue views}}",
                &[("list", "issue".into())],
            )
        });

    Button::new("issues-saved-views", Variant::Outline, Size::Default)
        .icon(Icon::Bookmark)
        .label(trigger_label)
        .tooltip(tooltip_text)
        .dropdown_menu(move |mut menu, _, cx| {
            for view in &views {
                let is_active = selected
                    .as_ref()
                    .is_some_and(|active| active.id == view.id && !modified);
                let view_id = view.id.clone();
                let view_prefs = view.preferences.normalized();
                let entity = entity.clone();
                menu = menu.item(
                    PopupMenuItem::new(view.name.clone())
                        .checked(is_active)
                        .on_click(move |_, window, cx| {
                            if let Some(page) = entity.upgrade() {
                                let view_id = view_id.clone();
                                let view_prefs = view_prefs.clone();
                                page.update(cx, |page, cx| {
                                    page.apply_saved_view(view_id, view_prefs, window, cx);
                                });
                            }
                        }),
                );
            }
            if !views.is_empty() {
                menu = menu.separator();
            }
            let save_entity = entity.clone();
            let suggested = issue_view_suggested_name(&prefs);
            menu = menu.item(
                PopupMenuItem::new(t(cx, "Save current view…").to_owned()).on_click(
                    move |_, window, cx| {
                        if let Some(page) = save_entity.upgrade() {
                            let initial = if suggested.is_empty() {
                                String::new()
                            } else {
                                t(cx, suggested).to_owned()
                            };
                            page.update(cx, |page, cx| {
                                page.open_saved_view_editor(None, initial, window, cx);
                            });
                        }
                    },
                ),
            );
            if let Some(active) = &selected {
                if modified {
                    let update_entity = entity.clone();
                    let active_id = active.id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(t(cx, "Update saved view").to_owned()).on_click(
                            move |_, _, cx| {
                                if let Some(page) = update_entity.upgrade() {
                                    let active_id = active_id.clone();
                                    page.update(cx, |page, cx| {
                                        page.update_active_saved_view(&active_id, cx);
                                    });
                                }
                            },
                        ),
                    );
                }
                let rename_entity = entity.clone();
                let active_id = active.id.clone();
                let active_name = active.name.clone();
                menu = menu.item(
                    PopupMenuItem::new(t(cx, "Rename view…").to_owned()).on_click(
                        move |_, window, cx| {
                            if let Some(page) = rename_entity.upgrade() {
                                let active_id = active_id.clone();
                                let active_name = active_name.clone();
                                page.update(cx, |page, cx| {
                                    page.open_saved_view_editor(
                                        Some(active_id),
                                        active_name,
                                        window,
                                        cx,
                                    );
                                });
                            }
                        },
                    ),
                );
                let delete_entity = entity.clone();
                let active_id = active.id.clone();
                menu = menu.item(
                    PopupMenuItem::new(t(cx, "Delete view").to_owned()).on_click(
                        move |_, _, cx| {
                            if let Some(page) = delete_entity.upgrade() {
                                let active_id = active_id.clone();
                                page.update(cx, |page, cx| {
                                    page.delete_active_saved_view(&active_id, cx);
                                });
                            }
                        },
                    ),
                );
            }
            menu
        })
}

fn host_filter_menu(
    page: &IssuesPage,
    entity: WeakEntity<IssuesPage>,
    cx: &App,
) -> Option<AnyElement> {
    let options = page.host_options();
    if options.len() <= 1 {
        return None;
    }
    let current = page.prefs.host.clone();
    let label = current
        .as_deref()
        .and_then(|host| {
            options
                .iter()
                .find(|opt| opt.host.as_deref() == Some(host))
                .map(|opt| opt.label.clone())
        })
        .unwrap_or_else(|| t(cx, "All").to_owned());

    Some(
        Button::new("issues-host-filter", Variant::Outline, Size::Default)
            .icon(Icon::Globe)
            .label(label)
            .tooltip(t(cx, "Filter by tracker").to_owned())
            .dropdown_menu(move |menu, _, _| {
                options.iter().fold(menu, |menu, opt| {
                    let host = opt.host.clone();
                    let checked = opt.host == current;
                    let unavailable = opt.unavailable.is_some();
                    let text = match &opt.unavailable {
                        Some(detail) => format!("{} · {detail}", opt.label),
                        None => opt.label.clone(),
                    };
                    let entity = entity.clone();
                    menu.item(
                        PopupMenuItem::new(text)
                            .checked(checked)
                            .disabled(unavailable)
                            .on_click(move |_, _, cx| {
                                let host = host.clone();
                                update(&entity, cx, move |prefs| prefs.host = host);
                            }),
                    )
                })
            })
            .into_any_element(),
    )
}

/// Modal dialog for saving or renaming a saved issue view (`SavedViewsMenu.tsx`).
pub fn render_saved_view_dialog(
    page: &IssuesPage,
    editor: &SavedViewEditor,
    cx: &mut Context<IssuesPage>,
) -> AnyElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let typed_name = editor.input.read(cx).value().to_string();
    let name_error =
        saved_view_name_error(&typed_name, &page.saved_views, editor.editing_id.as_deref());
    let title = if editor.editing_id.is_some() {
        crate::i18n::t_fmt(
            cx,
            "{list, select, pullRequest {Rename PR view} other {Rename issue view}}",
            &[("list", "issue".into())],
        )
    } else {
        crate::i18n::t_fmt(
            cx,
            "{list, select, pullRequest {Save PR view} other {Save issue view}}",
            &[("list", "issue".into())],
        )
    };
    let show_error = !typed_name.is_empty() && name_error.is_some();

    div()
        .id("issues-saved-view-backdrop")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(hsla(p.background).opacity(0.65))
        .on_click(cx.listener(|this, _, _, cx| this.close_saved_view_editor(cx)))
        .child(
            v_flex()
                .id("issues-saved-view-dialog")
                .w(px(380.))
                .max_w(px(420.))
                .p(px(20.))
                .gap(px(14.))
                .rounded(px(12.))
                .border_1()
                .border_color(hsla(p.border))
                .bg(hsla(p.popover))
                .shadow_lg()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    v_flex()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(hsla(p.popover_foreground))
                                .child(title.to_owned()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child(
                                    t(cx, "Save your search, filters, and sort order on this device.")
                                        .to_owned(),
                                ),
                        ),
                )
                .child(
                    v_flex()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(p.foreground))
                                .child(t(cx, "View name").to_owned()),
                        )
                        .child(
                            h_flex()
                                .h(px(34.))
                                .px(px(10.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(if show_error {
                                    hsla(p.destructive)
                                } else {
                                    hsla(p.input)
                                })
                                .bg(if dark {
                                    hsla(p.input).opacity(0.32)
                                } else {
                                    hsla(p.background)
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            Input::new(&editor.input)
                                                .appearance(false)
                                                .bordered(false),
                                        ),
                                ),
                        )
                        .when_some(
                            show_error.then_some(name_error).flatten(),
                            |this, error| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(hsla(p.destructive_foreground))
                                        .child(t(cx, error.message()).to_owned()),
                                )
                            },
                        ),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            Button::new("issues-view-cancel", Variant::Outline, Size::Sm)
                                .label(t(cx, "Cancel").to_owned())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_saved_view_editor(cx);
                                })),
                        )
                        .child(
                            Button::new("issues-view-save", Variant::Default, Size::Sm)
                                .label(t(cx, "Save").to_owned())
                                .disabled(name_error.is_some())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.submit_saved_view_editor(cx);
                                })),
                        ),
                ),
        )
        .into_any_element()
}
