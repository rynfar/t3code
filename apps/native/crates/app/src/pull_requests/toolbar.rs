//! The list's toolbar (`PullRequestsColumn.tsx`): the search field (`PullRequestSearchInput`),
//! the sort menu, the Filters menu (`PullRequestListFilters.tsx`) and refresh.

use gpui::{AnyElement, App, Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, WeakEntity, Window, div, prelude::*, px};
use gpui_component::input::Input;
use gpui_component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::issues::saved_views::{is_view_modified, resolve_selected_view, saved_view_name_error};
use t3_core::pull_requests::ListPreferences;
use t3_core::pull_requests::list::{Involvement, ListState, Sort, facets};
use t3_core::pull_requests::query::{ChecksFilter, DraftFilter, ReviewFilter, apply_draft_filter_change};
use t3_core::pull_requests::saved_views::{pull_request_view_matches, pull_request_view_suggested_name};
use t3_core::pull_requests::wire::{Actor, Label};

use super::controls::{Button, Size, Variant};
use super::page::{PullRequestsPage, SavedViewEditor};
use crate::icons::Icon;
use crate::theme::{hsla, palette};

/// How many author and label choices a submenu offers.
const CHOICES: usize = 24;

pub fn render(page: &mut PullRequestsPage, _window: &mut Window, cx: &mut Context<PullRequestsPage>) -> impl IntoElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let busy = !page.typed.trim().is_empty() && (page.typed.trim() != page.sent || page.data.pending);
    let entity = cx.entity().downgrade();
    let sort = page.prefs.sort;
    let sort_entity = entity.clone();
    let sort_menu = Button::new("pr-sort", Variant::Outline, Size::Icon).icon(Icon::ArrowDownUp).tooltip(format!("Sort: {}", sort.label())).dropdown_menu(move |menu, _, _| {
        Sort::ALL.iter().fold(menu, |menu, option| {
            let option = *option;
            let entity = sort_entity.clone();
            menu.item(PopupMenuItem::new(option.label()).checked(option == sort).on_click(move |_, _, cx| {
                update(&entity, cx, move |prefs| prefs.sort = option);
            }))
        })
    });
    let filters = filters_menu(page, entity.clone(), cx);
    let saved_views = saved_views_menu(page, entity.clone(), cx);
    let refresh = Button::new("pr-refresh", Variant::Outline, Size::Icon)
        .icon(Icon::RefreshCw)
        .busy(page.data.pending)
        .disabled(page.data.pending)
        .tooltip("Refresh pull requests")
        .on_click(cx.listener(|this, _, _, cx| this.refresh_from_host(cx)));
    h_flex()
        .flex_none()
        .gap(px(8.))
        .px(px(20.))
        .pt(px(16.))
        .pb(px(12.))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .h(px(32.))
                .px(px(10.))
                .gap(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(hsla(p.input))
                .bg(if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) })
                .child((if busy { Icon::LoaderCircle } else { Icon::Search }).sized(px(16.)).text_color(hsla(p.muted_foreground)))
                .child(div().flex_1().min_w_0().child(Input::new(&page.search).appearance(false).bordered(false))),
        )
        .child(sort_menu)
        .child(filters)
        .child(saved_views)
        .child(refresh)
}

fn update(entity: &WeakEntity<PullRequestsPage>, cx: &mut App, change: impl FnOnce(&mut ListPreferences) + 'static) {
    if let Some(entity) = entity.upgrade() {
        entity.update(cx, |page, cx| page.update_prefs(change, cx));
    }
}

struct Radio<T> {
    label: &'static str,
    options: Vec<(T, String)>,
    current: T,
    set: fn(&mut ListPreferences, T),
}

/// One radio submenu (`PullRequestFilterRadioSubmenu`): the trigger names its current value.
fn radio<T: Copy + PartialEq + 'static>(menu: PopupMenu, spec: Radio<T>, entity: &WeakEntity<PullRequestsPage>, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let Radio { label, options, current, set } = spec;
    let shown = options.iter().find(|(value, _)| *value == current).map_or(String::new(), |(_, label)| label.clone());
    let entity = entity.clone();
    menu.submenu(format!("{label} · {shown}"), window, cx, move |menu, _, _| {
        options.iter().fold(menu, |menu, (value, text)| {
            let value = *value;
            let entity = entity.clone();
            menu.item(PopupMenuItem::new(text.clone()).checked(value == current).on_click(move |_, _, cx| update(&entity, cx, move |prefs| set(prefs, value))))
        })
    })
}

fn filters_menu(page: &PullRequestsPage, entity: WeakEntity<PullRequestsPage>, cx: &App) -> impl IntoElement {
    let prefs = page.prefs.clone();
    let typed = page.typed.clone();
    let (author_facets, label_facets) = page.data.result.as_ref().map(|result| facets(&result.entries, ListState::All)).unwrap_or_default();
    let mut authors: Vec<Actor> = author_facets.into_iter().map(|facet| facet.actor).collect();
    for actor in &page.options.authors {
        if !authors.iter().any(|known| known.login.eq_ignore_ascii_case(&actor.login)) {
            authors.push(actor.clone());
        }
    }
    authors.truncate(CHOICES);
    let mut labels: Vec<Label> = label_facets.into_iter().map(|facet| facet.label).collect();
    for label in &page.options.labels {
        if !labels.iter().any(|known| known.name.eq_ignore_ascii_case(&label.name)) {
            labels.push(label.clone());
        }
    }
    labels.truncate(CHOICES);
    let hosts: Vec<String> = page.data.result.as_ref().map(|result| result.providers.iter().map(|provider| provider.host.clone()).collect()).unwrap_or_default();
    let unavailable: Vec<(String, String)> =
        page.data.result.as_ref().map(|result| result.errors.iter().map(|error| (error.project_id.clone(), error.message.clone())).collect()).unwrap_or_default();
    let projects: Vec<(String, String)> = page
        .state
        .read(cx)
        .shell
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.projects.iter().filter(|project| project.archived_at.is_none()).map(|project| (project.id.clone(), project.title.clone())).collect())
        .unwrap_or_default();
    Button::new("pr-filters", Variant::Outline, Size::Default).icon(Icon::ListFilter).label("Filters").count(prefs.filter_count()).dropdown_menu(move |menu, window, cx| {
        let menu = radio(menu, Radio { label: "State", options: ListState::ALL.iter().map(|s| (*s, s.label().to_owned())).collect(), current: prefs.state, set: |p, v| p.state = v }, &entity, window, cx);
        let menu = radio(menu, Radio { label: "Involvement", options: Involvement::ALL.iter().map(|i| (*i, i.label().to_owned())).collect(), current: prefs.involvement, set: |p, v| p.involvement = v }, &entity, window, cx)
            .separator();
        let author_options: Vec<(Option<usize>, String)> =
            std::iter::once((None, "All".to_owned())).chain(authors.iter().enumerate().map(|(index, actor)| (Some(index), actor.login.clone()))).collect();
        let current_author = prefs.author.as_ref().and_then(|author| authors.iter().position(|actor| actor.login.eq_ignore_ascii_case(author)));
        let author_entity = entity.clone();
        let author_logins: Vec<String> = authors.iter().map(|actor| actor.login.clone()).collect();
        let shown_author = prefs.author.clone().unwrap_or_else(|| "All".into());
        let author_set = prefs.author.is_some();
        let menu = menu.submenu(format!("Author · {shown_author}"), window, cx, move |menu, _, _| {
            author_options.iter().fold(menu, |menu, (index, text)| {
                let login = index.and_then(|index| author_logins.get(index).cloned());
                let entity = author_entity.clone();
                menu.item(PopupMenuItem::new(text.clone()).checked(match index {
                    None => !author_set,
                    Some(_) => *index == current_author,
                }).on_click(move |_, _, cx| {
                    let login = login.clone();
                    update(&entity, cx, move |prefs| prefs.author = login);
                }))
            })
        });
        let label_entity = entity.clone();
        let selected_labels = prefs.labels.clone();
        let label_names: Vec<String> = labels.iter().map(|label| label.name.clone()).collect();
        let shown_labels = if selected_labels.is_empty() { "All".to_owned() } else { selected_labels.len().to_string() };
        let menu = menu.submenu(format!("Labels · {shown_labels}"), window, cx, move |menu, _, _| {
            if label_names.is_empty() {
                return menu.item(PopupMenuItem::new("No labels found").disabled(true));
            }
            label_names.iter().fold(menu, |menu, name| {
                let checked = selected_labels.iter().any(|label| label.eq_ignore_ascii_case(name));
                let name = name.clone();
                let entity = label_entity.clone();
                menu.item(PopupMenuItem::new(name.clone()).checked(checked).on_click(move |_, _, cx| {
                    let name = name.clone();
                    update(&entity, cx, move |prefs| {
                        if let Some(index) = prefs.labels.iter().position(|label| label.eq_ignore_ascii_case(&name)) {
                            prefs.labels.remove(index);
                        } else if prefs.labels.len() < 10 {
                            prefs.labels.push(name);
                        }
                    });
                }))
            })
        });
        let draft_entity = entity.clone();
        let draft_typed = typed.clone();
        let draft_options: [(Option<DraftFilter>, &str); 3] = [(None, "All"), (Some(DraftFilter::Only), "Drafts only"), (Some(DraftFilter::Hide), "Hide drafts")];
        let draft_current = t3_core::pull_requests::query::parse_query(&typed).filters.draft.or(prefs.draft);
        let draft_label = draft_options.iter().find(|(value, _)| *value == draft_current).map_or("All", |(_, label)| label);
        let menu = menu.submenu(format!("Draft · {draft_label}"), window, cx, move |menu, _, _| {
            draft_options.iter().fold(menu, |menu, (value, text)| {
                let value = *value;
                let entity = draft_entity.clone();
                let typed = draft_typed.clone();
                menu.item(PopupMenuItem::new(*text).checked(value == draft_current).on_click(move |_, window, cx| {
                    if let Some(entity) = entity.upgrade() {
                        entity.update(cx, |page, cx| page.set_draft(&typed, value, window, cx));
                    }
                }))
            })
        });
        let review_options = vec![
            (None, "All".to_owned()),
            (Some(ReviewFilter::Approved), "Approved".to_owned()),
            (Some(ReviewFilter::ChangesRequested), "Changes requested".to_owned()),
            (Some(ReviewFilter::ReviewRequired), "Review required".to_owned()),
            (Some(ReviewFilter::None), "No reviews".to_owned()),
        ];
        let menu = radio(menu, Radio { label: "Review", options: review_options, current: prefs.review, set: |p, v| p.review = v }, &entity, window, cx);
        let checks_options = vec![(None, "All".to_owned()), (Some(ChecksFilter::Passing), "Passing".to_owned()), (Some(ChecksFilter::Failing), "Failing".to_owned())];
        let mut menu = radio(menu, Radio { label: "Checks", options: checks_options, current: prefs.checks, set: |p, v| p.checks = v }, &entity, window, cx);
        if hosts.len() > 1 {
            let host_options: Vec<(Option<usize>, String)> = std::iter::once((None, "All".to_owned())).chain(hosts.iter().enumerate().map(|(i, h)| (Some(i), h.clone()))).collect();
            let current = prefs.host.as_ref().and_then(|host| hosts.iter().position(|h| h == host));
            let names = hosts.clone();
            let host_entity = entity.clone();
            menu = menu.separator().submenu(format!("Host · {}", prefs.host.clone().unwrap_or_else(|| "All".into())), window, cx, move |menu, _, _| {
                host_options.iter().fold(menu, |menu, (index, text)| {
                    let host = index.and_then(|index| names.get(index).cloned());
                    let entity = host_entity.clone();
                    menu.item(PopupMenuItem::new(text.clone()).checked(*index == current).on_click(move |_, _, cx| {
                        let host = host.clone();
                        update(&entity, cx, move |prefs| prefs.host = host);
                    }))
                })
            });
        }
        let mut ordered = projects.clone();
        ordered.sort_by_key(|(id, _)| unavailable.iter().any(|(project, _)| project == id));
        let project_entity = entity.clone();
        let current_project = prefs.project_id.clone();
        let shown_project = current_project.as_ref().and_then(|id| projects.iter().find(|(pid, _)| pid == id)).map_or("All projects".to_owned(), |(_, title)| title.clone());
        let unavailable = unavailable.clone();
        menu.separator().submenu(format!("Project · {shown_project}"), window, cx, move |menu, _, _| {
            let entity = project_entity.clone();
            let menu = menu.item(PopupMenuItem::new("All projects").checked(current_project.is_none()).on_click(move |_, _, cx| update(&entity, cx, |prefs| prefs.project_id = None)));
            ordered.iter().fold(menu, |menu, (id, title)| {
                let entity = project_entity.clone();
                let id = id.clone();
                let blocked = unavailable.iter().any(|(project, _)| *project == id);
                let text: SharedString = if blocked { format!("{title} · Unavailable").into() } else { title.clone().into() };
                menu.item(PopupMenuItem::new(text).checked(current_project.as_ref() == Some(&id)).disabled(blocked).on_click(move |_, _, cx| {
                    let id = id.clone();
                    update(&entity, cx, move |prefs| prefs.project_id = Some(id));
                }))
            })
        })
    })
}

impl PullRequestsPage {
    /// The Draft menu takes a typed `draft:` qualifier out of the query when it changes it.
    fn set_draft(&mut self, typed: &str, draft: Option<DraftFilter>, window: &mut Window, cx: &mut Context<Self>) {
        let menu = self.prefs.menu_filters();
        let next = t3_core::pull_requests::query::ListFilters { draft, ..menu.clone() };
        let (query, filters) = apply_draft_filter_change(typed, &menu, &next);
        if query != typed {
            self.search.update(cx, |input, cx| input.set_value(query.clone(), window, cx));
        }
        self.update_prefs(move |prefs| prefs.draft = filters.draft, cx);
    }
}

/// Saved named pull request views menu (`SavedViewsMenu.tsx`, `pingdotgg/t3code#29387`).
fn saved_views_menu(page: &PullRequestsPage, entity: WeakEntity<PullRequestsPage>, _cx: &App) -> impl IntoElement {
    let views = page.saved_views.clone();
    let prefs = page.current_saved_preferences();
    let selected = resolve_selected_view(&views, page.selected_view_id.as_deref(), &prefs, pull_request_view_matches).cloned();
    let modified = is_view_modified(selected.as_ref(), &prefs, pull_request_view_matches);
    let trigger_label = match &selected {
        Some(view) if modified => format!("{}*", view.name),
        Some(view) => view.name.clone(),
        None => "Views".to_owned(),
    };
    let tooltip_text = selected
        .as_ref()
        .map(|view| view.name.clone())
        .unwrap_or_else(|| "Saved pull request views".to_owned());

    Button::new("pr-saved-views", Variant::Outline, Size::Default)
        .icon(Icon::Bookmark)
        .label(trigger_label)
        .tooltip(tooltip_text)
        .dropdown_menu(move |mut menu, _, _| {
            for view in &views {
                let is_active = selected.as_ref().is_some_and(|active| active.id == view.id && !modified);
                let view_id = view.id.clone();
                let view_prefs = view.preferences.normalized();
                let entity = entity.clone();
                menu = menu.item(PopupMenuItem::new(view.name.clone()).checked(is_active).on_click(move |_, window, cx| {
                    if let Some(page) = entity.upgrade() {
                        let view_id = view_id.clone();
                        let view_prefs = view_prefs.clone();
                        page.update(cx, |page, cx| page.apply_saved_view(view_id, view_prefs, window, cx));
                    }
                }));
            }
            if !views.is_empty() {
                menu = menu.separator();
            }
            let save_entity = entity.clone();
            let suggested = pull_request_view_suggested_name(&prefs);
            menu = menu.item(PopupMenuItem::new("Save current view…").on_click(move |_, window, cx| {
                if let Some(page) = save_entity.upgrade() {
                    let initial = suggested.to_owned();
                    page.update(cx, |page, cx| page.open_saved_view_editor(None, initial, window, cx));
                }
            }));
            if let Some(active) = &selected {
                if modified {
                    let update_entity = entity.clone();
                    let active_id = active.id.clone();
                    menu = menu.item(PopupMenuItem::new("Update saved view").on_click(move |_, _, cx| {
                        if let Some(page) = update_entity.upgrade() {
                            let active_id = active_id.clone();
                            page.update(cx, |page, cx| page.update_active_saved_view(&active_id, cx));
                        }
                    }));
                }
                let rename_entity = entity.clone();
                let active_id = active.id.clone();
                let active_name = active.name.clone();
                menu = menu.item(PopupMenuItem::new("Rename view…").on_click(move |_, window, cx| {
                    if let Some(page) = rename_entity.upgrade() {
                        let active_id = active_id.clone();
                        let active_name = active_name.clone();
                        page.update(cx, |page, cx| page.open_saved_view_editor(Some(active_id), active_name, window, cx));
                    }
                }));
                let delete_entity = entity.clone();
                let active_id = active.id.clone();
                menu = menu.item(PopupMenuItem::new("Delete view").on_click(move |_, _, cx| {
                    if let Some(page) = delete_entity.upgrade() {
                        let active_id = active_id.clone();
                        page.update(cx, |page, cx| page.delete_active_saved_view(&active_id, cx));
                    }
                }));
            }
            menu
        })
}

/// Modal dialog for saving or renaming a saved pull request view (`SavedViewsMenu.tsx`).
pub(super) fn render_saved_view_dialog(
    page: &PullRequestsPage,
    editor: &SavedViewEditor,
    cx: &mut Context<PullRequestsPage>,
) -> AnyElement {
    let p = palette(cx);
    let dark = cx.theme().is_dark();
    let typed_name = editor.input.read(cx).value().to_string();
    let name_error = saved_view_name_error(&typed_name, &page.saved_views, editor.editing_id.as_deref());
    let title = if editor.editing_id.is_some() {
        "Rename pull request view"
    } else {
        "Save pull request view"
    };
    let show_error = !typed_name.is_empty() && name_error.is_some();

    div()
        .id("pr-saved-view-backdrop")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(hsla(p.background).opacity(0.65))
        .on_click(cx.listener(|this, _, _, cx| this.close_saved_view_editor(cx)))
        .child(
            v_flex()
                .id("pr-saved-view-dialog")
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
                                .child(title),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(hsla(p.muted_foreground))
                                .child("Save your search, filters, and sort order on this device."),
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
                                .child("View name"),
                        )
                        .child(
                            h_flex()
                                .h(px(34.))
                                .px(px(10.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(if show_error { hsla(p.destructive) } else { hsla(p.input) })
                                .bg(if dark { hsla(p.input).opacity(0.32) } else { hsla(p.background) })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(Input::new(&editor.input).appearance(false).bordered(false)),
                                ),
                        )
                        .when_some(show_error.then_some(name_error).flatten(), |this, error| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(hsla(p.destructive_foreground))
                                    .child(error.message()),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(
                            Button::new("pr-view-cancel", Variant::Outline, Size::Sm)
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| this.close_saved_view_editor(cx))),
                        )
                        .child(
                            Button::new("pr-view-save", Variant::Default, Size::Sm)
                                .label("Save")
                                .disabled(name_error.is_some())
                                .on_click(cx.listener(|this, _, _, cx| this.submit_saved_view_editor(cx))),
                        ),
                ),
        )
        .into_any_element()
}
