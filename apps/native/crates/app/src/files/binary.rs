//! Binary / Hex viewer (bounded hex + ASCII with offset ruler) and ZIP/TAR archive inspector.
//! Ports `apps/web/src/components/files/BinaryViewer.tsx` and `binaryPreview.worker.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, FontWeight, IntoElement, ParentElement, ScrollHandle, SharedString,
    Styled, UniformListScrollHandle, div, prelude::*, px, uniform_list,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::files::binary::{
    ArchiveEntry, BINARY_PREVIEW_MAX_BYTES, HexDocument, archive_children, archive_parent_folder,
    list_tar_entries, list_zip_entries, read_zip_entry,
};
use t3_core::files::{
    BinaryContainerKind, CREATE_ASSET_URL, absolute_path, binary_container_kind, decode_asset_url,
    group_thousands, workspace_asset_request,
};

use super::Root;
use super::preview::Preview;
use crate::app_state::AppState;
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

const HEX_ROW_HEIGHT: f32 = 20.;
const ARCHIVE_ROW_HEIGHT: f32 = 28.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryViewMode {
    Hex,
    Archive,
}

pub enum ArchiveData {
    Zip { raw: Arc<Vec<u8>>, entries: Vec<ArchiveEntry> },
    Tar { raw: Arc<Vec<u8>>, entries: Vec<ArchiveEntry>, ranges: BTreeMap<String, (usize, usize)> },
}

impl ArchiveData {
    pub fn entries(&self) -> &[ArchiveEntry] {
        match self {
            ArchiveData::Zip { entries, .. } | ArchiveData::Tar { entries, .. } => entries,
        }
    }

    pub fn extract(&self, path: &str) -> Result<Vec<u8>, String> {
        match self {
            ArchiveData::Zip { raw, .. } => read_zip_entry(raw, path),
            ArchiveData::Tar { raw, ranges, .. } => {
                let key = path.trim_end_matches('/');
                let &(start, end) = ranges
                    .get(key)
                    .ok_or_else(|| "This archive entry is unavailable.".to_owned())?;
                if end - start > t3_core::files::binary::ARCHIVE_ENTRY_MAX_BYTES {
                    return Err("Entry previews are limited to 10 MB.".to_owned());
                }
                Ok(raw[start..end].to_vec())
            }
        }
    }
}

pub struct BinaryReady {
    pub hex: HexDocument,
    pub archive: Option<ArchiveData>,
    pub mode: BinaryViewMode,
    pub folder: String,
    pub selected_entry: Option<(String, Result<String, String>)>,
    pub hex_scroll: UniformListScrollHandle,
    pub archive_scroll: UniformListScrollHandle,
    pub entry_scroll: ScrollHandle,
}

pub enum BinaryState {
    Loading,
    Ready(Box<BinaryReady>),
    Failed(String),
}

pub struct DecodedBinary {
    pub hex: HexDocument,
    pub archive: Option<ArchiveData>,
}

/// Downloads binary bytes via signed asset URL on the network runtime.
pub fn load(
    state: &AppState,
    root: &Root,
    path: &str,
    mutation: Option<&str>,
) -> Option<tokio::task::JoinHandle<Result<Vec<u8>, String>>> {
    let client = state.client_handle()?;
    let request = workspace_asset_request(
        &root.workspace.thread_id,
        &root.workspace.cwd,
        &absolute_path(path, &root.workspace.cwd),
    );
    let mutation = mutation.map(str::to_owned);
    Some(state.runtime().spawn(async move {
        let value = client
            .request(CREATE_ASSET_URL, request)
            .await
            .map_err(|_| "Could not read this binary file.".to_owned())?;
        let url = decode_asset_url(&value).map_err(|_| "Could not read this binary file.".to_owned())?;
        client
            .asset_bytes(
                &format!("{url}{}", t3_core::media::workspace_image_suffix(&url, mutation.as_deref())),
                BINARY_PREVIEW_MAX_BYTES,
            )
            .await
    }))
}

/// Builds bounded hex + optional archive index off the UI thread.
pub fn decode(path: &str, bytes: Vec<u8>) -> DecodedBinary {
    let hex = HexDocument::from_bytes(&bytes, None);
    let is_archive_ext = binary_container_kind(path) == Some(BinaryContainerKind::Archive);
    let is_zip = bytes.starts_with(&[0x50, 0x4b, 0x03, 0x04]) || bytes.starts_with(&[0x50, 0x4b, 0x05, 0x06]);
    let is_tar = bytes.len() >= 263 && &bytes[257..262] == b"ustar";

    let archive = if is_zip {
        list_zip_entries(&bytes)
            .ok()
            .map(|entries| ArchiveData::Zip { raw: Arc::new(bytes), entries })
    } else if is_tar || (is_archive_ext && path.to_ascii_lowercase().ends_with(".tar")) {
        list_tar_entries(&bytes)
            .ok()
            .map(|(entries, ranges)| ArchiveData::Tar { raw: Arc::new(bytes), entries, ranges })
    } else {
        None
    };

    DecodedBinary { hex, archive }
}

impl From<Result<DecodedBinary, String>> for BinaryState {
    fn from(result: Result<DecodedBinary, String>) -> Self {
        match result {
            Ok(decoded) => {
                let mode = if decoded.archive.is_some() {
                    BinaryViewMode::Archive
                } else {
                    BinaryViewMode::Hex
                };
                BinaryState::Ready(Box::new(BinaryReady {
                    hex: decoded.hex,
                    archive: decoded.archive,
                    mode,
                    folder: String::new(),
                    selected_entry: None,
                    hex_scroll: UniformListScrollHandle::new(),
                    archive_scroll: UniformListScrollHandle::new(),
                    entry_scroll: ScrollHandle::new(),
                }))
            }
            Err(error) => BinaryState::Failed(error),
        }
    }
}

impl BinaryState {
    pub fn failed(&self) -> bool {
        matches!(self, BinaryState::Failed(_))
    }

    pub fn set_mode(&mut self, next: BinaryViewMode) {
        if let BinaryState::Ready(ready) = self
            && (next == BinaryViewMode::Hex || ready.archive.is_some())
        {
            ready.mode = next;
        }
    }

    pub fn open_folder(&mut self, next_folder: String) {
        if let BinaryState::Ready(ready) = self {
            ready.folder = next_folder;
            ready.selected_entry = None;
        }
    }

    pub fn go_up(&mut self) {
        if let BinaryState::Ready(ready) = self {
            if ready.selected_entry.is_some() {
                ready.selected_entry = None;
            } else {
                ready.folder = archive_parent_folder(&ready.folder);
            }
        }
    }

    pub fn open_entry(&mut self, entry_path: String) {
        if let BinaryState::Ready(ready) = self
            && let Some(archive) = &ready.archive
        {
            let preview = archive.extract(&entry_path).and_then(|bytes| {
                let limit = bytes.len().min(1024 * 1024);
                let slice = &bytes[..limit];
                if slice.contains(&0) {
                    return Err("Only UTF-8 text entries can be previewed inside an archive.".to_owned());
                }
                std::str::from_utf8(slice)
                    .map(str::to_owned)
                    .map_err(|_| "Only UTF-8 text entries can be previewed inside an archive.".to_owned())
            });
            ready.selected_entry = Some((entry_path, preview));
        }
    }
}

impl Preview {
    pub(super) fn render_binary(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let entity = cx.entity().downgrade();

        let Some(binary) = self.binary_state() else {
            return div().into_any_element();
        };

        match binary {
            BinaryState::Loading => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child("Loading binary preview…")
                .into_any_element(),
            BinaryState::Failed(error) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .px(px(24.))
                .text_center()
                .text_xs()
                .text_color(hsla(p.destructive))
                .child(error.clone())
                .into_any_element(),
            BinaryState::Ready(ready) => {
                let BinaryReady {
                    hex,
                    archive,
                    mode,
                    folder,
                    selected_entry,
                    hex_scroll,
                    archive_scroll,
                    entry_scroll,
                } = ready.as_ref();
                let has_archive = archive.is_some();
                let mode = *mode;
                let signature = hex.signature;
                let total_bytes = hex.total_bytes;
                let truncated = hex.truncated;
                let row_count = hex.row_count();
                let sqlite = hex.sqlite.clone();
                let folder_str = folder.clone();
                let entry_count = archive.as_ref().map_or(0, |a| a.entries().len());
                let hex_scroll = hex_scroll.clone();
                let archive_scroll = archive_scroll.clone();
                let entry_scroll = entry_scroll.clone();
                let selected_entry_info = selected_entry.as_ref().map(|(path, res)| (path.clone(), res.clone()));
                let children_len = archive
                    .as_ref()
                    .map_or(0, |a| archive_children(a.entries(), &folder_str).len());

                let header = h_flex()
                    .flex_none()
                    .min_h(px(36.))
                    .px(px(12.))
                    .py(px(6.))
                    .gap(px(8.))
                    .items_center()
                    .border_b_1()
                    .border_color(hsla(p.border).opacity(0.6))
                    .bg(hsla(p.background))
                    .text_xs()
                    .child(
                        div()
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .bg(hsla(p.secondary))
                            .border_1()
                            .border_color(hsla(p.border))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child(signature),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(hsla(p.muted_foreground))
                            .child(if has_archive {
                                format!(
                                    "{} entries · {} bytes",
                                    group_thousands(entry_count as u64),
                                    group_thousands(total_bytes)
                                )
                            } else {
                                format!("{} bytes", group_thousands(total_bytes))
                            }),
                    )
                    .when(has_archive, |this| {
                        this.child(
                            Button::new("binary-mode-archive")
                                .variant(Variant::Toggle)
                                .size(Size::Xs)
                                .label("Archive")
                                .pressed(mode == BinaryViewMode::Archive)
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        if let Some(entity) = entity.upgrade() {
                                            entity.update(cx, |this, cx| {
                                                this.set_binary_mode(BinaryViewMode::Archive, cx);
                                            });
                                        }
                                    }
                                }),
                        )
                        .child(
                            Button::new("binary-mode-hex")
                                .variant(Variant::Toggle)
                                .size(Size::Xs)
                                .label("Hex")
                                .pressed(mode == BinaryViewMode::Hex)
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        if let Some(entity) = entity.upgrade() {
                                            entity.update(cx, |this, cx| {
                                                this.set_binary_mode(BinaryViewMode::Hex, cx);
                                            });
                                        }
                                    }
                                }),
                        )
                    });

                let body: AnyElement = match mode {
                    BinaryViewMode::Hex => v_flex()
                        .id("file-binary-hex")
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .bg(hsla(p.code_background))
                        .when_some(sqlite, |this, info| {
                            this.child(
                                div()
                                    .flex_none()
                                    .border_b_1()
                                    .border_color(hsla(p.border).opacity(0.6))
                                    .bg(hsla(p.background))
                                    .px(px(12.))
                                    .py(px(6.))
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(format!(
                                        "SQLite 3 · {} B pages · {} pages · {} · schema v{} · user_version {}",
                                        group_thousands(u64::from(info.page_size)),
                                        group_thousands(u64::from(info.page_count)),
                                        info.text_encoding,
                                        info.schema_format,
                                        info.user_version
                                    )),
                            )
                        })
                        .when(truncated, |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .border_b_1()
                                    .border_color(hsla(p.warning).opacity(0.2))
                                    .bg(hsla(p.warning_surface))
                                    .px(px(12.))
                                    .py(px(4.))
                                    .text_size(px(11.))
                                    .text_color(hsla(p.warning_foreground))
                                    .child(format!(
                                        "Hex preview limited to the first 256 KB of a {} byte file.",
                                        group_thousands(total_bytes)
                                    )),
                            )
                        })
                        .child(
                            h_flex()
                                .flex_none()
                                .h(px(24.))
                                .px(px(12.))
                                .gap(px(16.))
                                .items_center()
                                .border_b_1()
                                .border_color(hsla(p.border).opacity(0.6))
                                .bg(hsla(p.muted))
                                .font_family(mono.clone())
                                .text_size(px(11.))
                                .text_color(hsla(p.muted_foreground))
                                .child(div().w(px(72.)).child("Offset"))
                                .child(div().w(px(176.)).child("00 01 02 03 04 05 06 07"))
                                .child(div().w(px(176.)).child("08 09 0a 0b 0c 0d 0e 0f"))
                                .child(div().flex_1().child("ASCII")),
                        )
                        .child(
                            div()
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .child(
                                    uniform_list(
                                        "binary-hex-rows",
                                        row_count,
                                        cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                            range.map(|index| this.render_hex_row(index, cx)).collect::<Vec<_>>()
                                        }),
                                    )
                                    .size_full()
                                    .py(px(4.))
                                    .track_scroll(&hex_scroll),
                                )
                                .vertical_scrollbar(&hex_scroll),
                        )
                        .into_any_element(),
                    BinaryViewMode::Archive => {
                        let nav_bar = h_flex()
                            .flex_none()
                            .h(px(34.))
                            .px(px(12.))
                            .gap(px(8.))
                            .items_center()
                            .border_b_1()
                            .border_color(hsla(p.border).opacity(0.6))
                            .text_xs()
                            .child(
                                Button::new("archive-back")
                                    .variant(Variant::Ghost)
                                    .size(Size::IconXs)
                                    .icon(Icon::ChevronLeft)
                                    .disabled(selected_entry_info.is_none() && folder_str.is_empty())
                                    .tooltip(if selected_entry_info.is_some() {
                                        "Back to archive"
                                    } else {
                                        "Parent folder"
                                    })
                                    .on_click({
                                        let entity = entity.clone();
                                        move |_, _, cx| {
                                            if let Some(entity) = entity.upgrade() {
                                                entity.update(cx, |this, cx| this.archive_go_up(cx));
                                            }
                                        }
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(hsla(p.foreground))
                                    .child(SharedString::from(
                                        selected_entry_info
                                            .as_ref()
                                            .map(|(path, _)| path.clone())
                                            .unwrap_or_else(|| {
                                                if folder_str.is_empty() {
                                                    "Archive contents".to_owned()
                                                } else {
                                                    folder_str.clone()
                                                }
                                            }),
                                    )),
                            );

                        let content = if let Some((_, result)) = selected_entry_info {
                            match result {
                                Ok(text) => div()
                                    .relative()
                                    .flex_1()
                                    .min_h_0()
                                    .bg(hsla(p.code_background))
                                    .child(
                                        div()
                                            .id("archive-entry-scroll")
                                            .size_full()
                                            .overflow_y_scroll()
                                            .track_scroll(&entry_scroll)
                                            .p(px(12.))
                                            .font_family(mono.clone())
                                            .text_size(cx.theme().mono_font_size)
                                            .text_color(hsla(p.code_foreground))
                                            .child(SharedString::from(text)),
                                    )
                                    .vertical_scrollbar(&entry_scroll)
                                    .into_any_element(),
                                Err(err) => div()
                                    .flex_1()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .p(px(24.))
                                    .text_xs()
                                    .text_color(hsla(p.muted_foreground))
                                    .child(err)
                                    .into_any_element(),
                            }
                        } else {
                            div()
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .child(
                                    uniform_list(
                                        "archive-entries",
                                        children_len,
                                        cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                            range.map(|index| this.render_archive_row(index, cx)).collect::<Vec<_>>()
                                        }),
                                    )
                                    .size_full()
                                    .track_scroll(&archive_scroll),
                                )
                                .vertical_scrollbar(&archive_scroll)
                                .into_any_element()
                        };

                        v_flex().id("file-binary-archive").flex_1().min_h_0().child(nav_bar).child(content).into_any_element()
                    }
                };

                v_flex().flex_1().min_h_0().min_w_0().child(header).child(body).into_any_element()
            }
        }
    }

    fn render_hex_row(&self, index: usize, cx: &App) -> AnyElement {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let Some(BinaryState::Ready(ready)) = self.binary_state() else {
            return div().into_any_element();
        };
        let Some(row) = ready.hex.row(index) else {
            return div().into_any_element();
        };
        h_flex()
            .h(px(HEX_ROW_HEIGHT))
            .w_full()
            .px(px(12.))
            .gap(px(16.))
            .items_center()
            .font_family(mono)
            .text_size(px(12.))
            .when(index % 2 == 1, |this| this.bg(hsla(p.muted).opacity(0.35)))
            .child(
                div()
                    .flex_none()
                    .w(px(72.))
                    .text_color(hsla(p.muted_foreground))
                    .child(SharedString::from(row.offset_label)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(176.))
                    .text_color(hsla(p.code_foreground))
                    .child(SharedString::from(row.hex_left)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(176.))
                    .text_color(hsla(p.code_foreground))
                    .child(SharedString::from(row.hex_right)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(hsla(p.primary))
                    .child(SharedString::from(row.ascii)),
            )
            .into_any_element()
    }

    fn render_archive_row(&self, index: usize, cx: &Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(BinaryState::Ready(ready)) = self.binary_state() else {
            return div().into_any_element();
        };
        let Some(archive) = &ready.archive else {
            return div().into_any_element();
        };
        let children = archive_children(archive.entries(), &ready.folder);
        let Some(child) = children.get(index).cloned() else {
            return div().into_any_element();
        };
        let path = child.path.clone();
        let directory = child.directory;
        h_flex()
            .id(("archive-row", index))
            .h(px(ARCHIVE_ROW_HEIGHT))
            .w_full()
            .px(px(12.))
            .gap(px(8.))
            .items_center()
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.4))
            .text_xs()
            .cursor_pointer()
            .hover(|s| s.bg(hsla(p.accent)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if directory {
                    this.archive_open_folder(format!("{path}/"), cx);
                } else {
                    this.archive_open_entry(path.clone(), cx);
                }
            }))
            .child(
                (if directory { Icon::Folder } else { Icon::File })
                    .svg()
                    .flex_none()
                    .size(px(14.))
                    .text_color(hsla(p.icon_muted)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(hsla(p.foreground))
                    .child(SharedString::from(child.name)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(hsla(p.muted_foreground))
                    .child(format!("{} B", group_thousands(child.size))),
            )
            .when_some(child.modified, |this, modified| {
                this.child(
                    div()
                        .flex_none()
                        .w(px(110.))
                        .text_right()
                        .text_color(hsla(p.muted_foreground))
                        .child(SharedString::from(modified)),
                )
            })
            .into_any_element()
    }
}
