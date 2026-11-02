//! The list's toolbar (`PullRequestsColumn.tsx`): the search field (`PullRequestSearchInput`),
//! the sort menu, the Filters menu (`PullRequestListFilters.tsx`) and refresh.

use gpui::{App, Context, IntoElement, ParentElement, SharedString, Styled, WeakEntity, Window, div, px};
use gpui_component::input::Input;
use gpui_component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, h_flex};
use t3_core::pull_requests::ListPreferences;
use t3_core::pull_requests::list::{Involvement, ListState, Sort, facets};
use t3_core::pull_requests::query::{ChecksFilter, DraftFilter, ReviewFilter, apply_draft_filter_change};
use t3_core::pull_requests::wire::{Actor, Label};

use super::controls::{Button, Size, Variant};
use super::page::PullRequestsPage;
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
