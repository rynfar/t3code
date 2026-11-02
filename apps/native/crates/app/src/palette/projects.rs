//! The palette's Add project flows (`CommandPalette.tsx` browse mode, `filesystem.ts`): a typed
//! path lists the host's folders from `filesystem.browse`, Enter enters a folder or adds the
//! path, Tab completes; and New project names a folder for `projects.createNew`.

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, Styled, Task, UniformListScrollHandle, div, prelude::*, px, uniform_list};
use t3_core::projects::{BrowseResult, FILESYSTEM_BROWSE, add_target, append_browse_segment, browse_path, browse_request, browse_tab_completion, filter_browse_entries};

use super::CommandPalette;
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::theme::{hsla, palette};

const ROW_HEIGHT: f32 = 32.;

#[derive(Default)]
pub(super) struct ProjectBrowse {
    query: String,
    loaded_directory: Option<String>,
    result: Option<BrowseResult>,
    pending: bool,
    error: Option<String>,
    /// Row 0 adds the typed path; rows after it are folders.
    pub highlighted: usize,
    pub scroll: UniformListScrollHandle,
    request: Option<Task<()>>,
}

/// What Enter does in the browse list.
pub(super) enum BrowsePick {
    Add(String),
    Enter(String),
}

impl ProjectBrowse {
    pub fn reset(&mut self) {
        *self = ProjectBrowse::default();
    }

    pub fn set_query(&mut self, query: String, state: &Entity<AppState>, cx: &mut Context<CommandPalette>) {
        self.query = query;
        self.highlighted = 0;
        let Some(path) = browse_path(&self.query, false) else {
            self.result = None;
            self.loaded_directory = None;
            return;
        };
        if self.loaded_directory.as_deref() == Some(path.directory.as_str()) {
            return;
        }
        self.loaded_directory = Some(path.directory.clone());
        self.pending = true;
        let request = state.read(cx).rpc(FILESYSTEM_BROWSE, browse_request(&path.directory, None));
        self.request = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |palette, cx| {
                let browse = &mut palette.browse;
                browse.pending = false;
                match result.map(serde_json::from_value::<BrowseResult>) {
                    Ok(Ok(result)) => {
                        browse.result = Some(result);
                        browse.error = None;
                    }
                    Ok(Err(error)) => browse.error = Some(error.to_string()),
                    Err(error) => browse.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
    }

    fn filter(&self) -> String {
        browse_path(&self.query, false).map(|path| path.filter).unwrap_or_default()
    }

    fn visible_names(&self) -> Vec<(String, String)> {
        let filter = self.filter();
        self.result.as_ref().map(|result| filter_browse_entries(&result.entries, &filter).into_iter().map(|entry| (entry.name.clone(), entry.full_path.clone())).collect()).unwrap_or_default()
    }

    fn row_count(&self) -> usize {
        usize::from(browse_path(&self.query, false).is_some()) + self.visible_names().len()
    }

    pub fn move_highlight(&mut self, step: isize) {
        let count = self.row_count() as isize;
        if count > 0 {
            self.highlighted = (self.highlighted as isize + step).rem_euclid(count) as usize;
            self.scroll.scroll_to_item(self.highlighted, gpui::ScrollStrategy::Top);
        }
    }

    /// Enter: add the typed path from the first row, else enter the highlighted folder.
    /// With `add`, a folder row is added instead of entered.
    pub fn pick(&self, add: bool) -> Option<BrowsePick> {
        browse_path(&self.query, false)?;
        if self.highlighted == 0 {
            return Some(BrowsePick::Add(add_target(&self.query)));
        }
        let (name, full_path) = self.visible_names().into_iter().nth(self.highlighted - 1)?;
        Some(if add { BrowsePick::Add(full_path) } else { BrowsePick::Enter(append_browse_segment(&self.query, &name)) })
    }

    /// Tab: the new query, when completion applies.
    pub fn complete(&self) -> Option<String> {
        let result = self.result.as_ref()?;
        let filter = self.filter();
        let visible = filter_browse_entries(&result.entries, &filter);
        let (name, complete) = browse_tab_completion(&visible, &filter)?;
        Some(if complete { append_browse_segment(&self.query, &name) } else { format!("{}{name}", t3_core::projects::browse_directory_path(&self.query)) })
    }

    pub fn render(&self, cx: &mut Context<CommandPalette>) -> AnyElement {
        let p = palette(cx);
        if browse_path(&self.query, false).is_none() {
            return div().py(px(32.)).px(px(24.)).flex().justify_center().text_sm().text_color(hsla(p.muted_foreground)).child("Enter project path (e.g. ~/projects/my-app)").into_any_element();
        }
        let count = self.row_count();
        div()
            .flex()
            .flex_col()
            .min_h_0()
            .p(px(8.))
            .children(self.error.clone().map(|error| div().px(px(8.)).py(px(4.)).text_xs().text_color(hsla(p.destructive)).child(error)))
            .when(self.pending && self.result.is_none(), |this| this.child(div().px(px(8.)).py(px(4.)).text_xs().text_color(hsla(p.muted_foreground)).child("Loading folders…")))
            .child(
                uniform_list("palette-browse", count, cx.processor(|this, range: std::ops::Range<usize>, _, cx| range.map(|index| this.browse.render_row(index, cx)).collect::<Vec<_>>()))
                    .track_scroll(&self.scroll)
                    .h(px((count as f32 * ROW_HEIGHT).min(420.))),
            )
            .into_any_element()
    }

    fn render_row(&self, index: usize, cx: &mut Context<CommandPalette>) -> AnyElement {
        let p = palette(cx);
        let (icon, title, detail) = if index == 0 {
            (Icon::FolderTree, format!("Add {}", add_target(&self.query)), String::new())
        } else {
            let Some((name, full_path)) = self.visible_names().into_iter().nth(index - 1) else { return div().into_any_element() };
            (Icon::Folder, name, full_path)
        };
        div()
            .id(("palette-browse-row", index))
            .h(px(ROW_HEIGHT))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(4.))
            .cursor_pointer()
            .text_sm()
            .when(index == self.highlighted, |this| this.bg(hsla(p.foreground).opacity(0.09)))
            .child(icon.render(px(16.), hsla(p.icon_muted)))
            .child(div().flex_none().text_color(hsla(p.foreground)).font_weight(if index == 0 { FontWeight::MEDIUM } else { FontWeight::NORMAL }).child(title))
            .child(div().flex_1().min_w_0().truncate().text_xs().text_color(hsla(p.muted_foreground)).child(detail))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.browse.highlighted != index {
                    this.browse.highlighted = index;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.browse.highlighted = index;
                this.browse_pick(event.modifiers().platform, window, cx);
            }))
            .into_any_element()
    }
}
