//! Binary / Hex viewer (bounded hex + ASCII with offset ruler), ZIP/TAR archive inspector, and
//! read-only SQLite table & paged row viewer.
//! Ports `apps/web/src/components/files/BinaryViewer.tsx`, `binaryPreview.worker.ts`, and
//! `binaryPreviewProtocol.ts`.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, FontWeight, IntoElement, ParentElement, Pixels, ScrollHandle,
    ScrollStrategy, ScrollWheelEvent, SharedString, Styled, Task, UniformListScrollHandle, Window,
    canvas, div, prelude::*, px, uniform_list,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, h_flex, v_flex};
use t3_core::files::binary::{
    ArchiveEntry, BINARY_PREVIEW_MAX_BYTES, HexDocument, archive_children, archive_parent_folder,
    list_tar_entries, list_zip_entries, read_zip_entry,
};
use t3_core::files::sqlite::{
    SQLITE_PAGE_SIZE, SqliteCellKind, SqliteObjectKind, SqlitePageData, SqliteTableInfo,
    estimate_sqlite_column_width, sqlite_page_range,
};
use t3_core::files::{
    BinaryContainerKind, CREATE_ASSET_URL, absolute_path, binary_container_kind, decode_asset_url,
    group_thousands, workspace_asset_request,
};
use t3_core::i18n::IcuArg;

use super::Root;
use super::preview::Preview;
use super::sqlite::SqliteDatabaseHandle;
use crate::app_state::AppState;
use crate::i18n::{t, t_fmt};
use crate::icons::Icon;
use crate::right_panel::{Button, Size, Variant};
use crate::theme::{hsla, palette};

const HEX_ROW_HEIGHT: f32 = 20.;
const ARCHIVE_ROW_HEIGHT: f32 = 28.;
const SQLITE_HEADER_HEIGHT: f32 = 30.;
const SQLITE_ROW_HEIGHT: f32 = 28.;
const SQLITE_GUTTER_WIDTH: f32 = 56.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryViewMode {
    Hex,
    Archive,
    Sqlite,
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

pub enum SqlitePageState {
    Loading,
    Ready(SqlitePageData),
    Failed(String),
}

pub struct SqliteViewer {
    pub db: SqliteDatabaseHandle,
    pub selected_table: Option<String>,
    pub page: usize,
    pub page_state: SqlitePageState,
    pub query_generation: u64,
    pub query_task: Option<Task<()>>,
    pub column_widths: Vec<f32>,
    pub h_offset: Pixels,
    pub viewport_width: Rc<Cell<f32>>,
    pub rows_scroll: UniformListScrollHandle,
    pub tables_scroll: ScrollHandle,
}

impl SqliteViewer {
    fn new(db: SqliteDatabaseHandle, initial_page: Option<Result<SqlitePageData, String>>) -> Self {
        let selected_table = db.tables().first().map(|table| table.name.clone());
        let (page_state, column_widths) = match initial_page {
            Some(Ok(page_data)) => {
                let widths = compute_column_widths(&page_data);
                (SqlitePageState::Ready(page_data), widths)
            }
            Some(Err(error)) => (SqlitePageState::Failed(error), Vec::new()),
            None => (SqlitePageState::Loading, Vec::new()),
        };
        Self {
            db,
            selected_table,
            page: 0,
            page_state,
            query_generation: 0,
            query_task: None,
            column_widths,
            h_offset: px(0.),
            viewport_width: Rc::new(Cell::new(0.)),
            rows_scroll: UniformListScrollHandle::new(),
            tables_scroll: ScrollHandle::new(),
        }
    }

    pub fn active_table(&self) -> Option<&SqliteTableInfo> {
        let name = self.selected_table.as_deref()?;
        self.db.table(name)
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.page_state, SqlitePageState::Loading) && self.selected_table.is_some()
    }

    pub fn apply_page_result(&mut self, generation: u64, result: Result<SqlitePageData, String>) {
        if self.query_generation != generation {
            return;
        }
        self.query_task = None;
        match result {
            Ok(data) => {
                self.page = data.page;
                self.column_widths = compute_column_widths(&data);
                self.h_offset = px(0.);
                self.rows_scroll.scroll_to_item(0, ScrollStrategy::Top);
                self.page_state = SqlitePageState::Ready(data);
            }
            Err(error) => {
                self.page_state = SqlitePageState::Failed(error);
            }
        }
    }

    fn total_grid_width(&self) -> f32 {
        SQLITE_GUTTER_WIDTH + self.column_widths.iter().sum::<f32>()
    }
}

fn compute_column_widths(data: &SqlitePageData) -> Vec<f32> {
    data.columns
        .iter()
        .enumerate()
        .map(|(col_idx, col)| estimate_sqlite_column_width(col, &data.rows, col_idx))
        .collect()
}

pub enum SqliteData {
    Ready(Box<SqliteViewer>),
    Failed(String),
}

pub enum DecodedSqlite {
    Ready { db: SqliteDatabaseHandle, initial_page: Option<Result<SqlitePageData, String>> },
    Failed(String),
}

pub struct BinaryReady {
    pub hex: HexDocument,
    pub archive: Option<ArchiveData>,
    pub sqlite: Option<SqliteData>,
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
    pub sqlite: Option<DecodedSqlite>,
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

/// Builds bounded hex + optional archive index or read-only SQLite handle off the UI thread.
pub fn decode(path: &str, bytes: Vec<u8>) -> DecodedBinary {
    let hex = HexDocument::from_bytes(&bytes, None);
    let container_kind = binary_container_kind(path);
    let is_sqlite =
        container_kind == Some(BinaryContainerKind::Sqlite) || bytes.starts_with(b"SQLite format 3\0");
    if is_sqlite {
        let sqlite = match SqliteDatabaseHandle::open(&bytes) {
            Ok((db, initial_page)) => DecodedSqlite::Ready { db, initial_page },
            Err(error) => DecodedSqlite::Failed(error),
        };
        return DecodedBinary {
            hex,
            archive: None,
            sqlite: Some(sqlite),
        };
    }

    let is_archive_ext = container_kind == Some(BinaryContainerKind::Archive);
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

    DecodedBinary { hex, archive, sqlite: None }
}

impl From<Result<DecodedBinary, String>> for BinaryState {
    fn from(result: Result<DecodedBinary, String>) -> Self {
        match result {
            Ok(decoded) => {
                let sqlite = decoded.sqlite.map(|decoded_sqlite| match decoded_sqlite {
                    DecodedSqlite::Ready { db, initial_page } => {
                        SqliteData::Ready(Box::new(SqliteViewer::new(db, initial_page)))
                    }
                    DecodedSqlite::Failed(error) => SqliteData::Failed(error),
                });
                let mode = if sqlite.is_some() {
                    BinaryViewMode::Sqlite
                } else if decoded.archive.is_some() {
                    BinaryViewMode::Archive
                } else {
                    BinaryViewMode::Hex
                };
                BinaryState::Ready(Box::new(BinaryReady {
                    hex: decoded.hex,
                    archive: decoded.archive,
                    sqlite,
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
        if let BinaryState::Ready(ready) = self {
            let allowed = match next {
                BinaryViewMode::Hex => true,
                BinaryViewMode::Archive => ready.archive.is_some(),
                BinaryViewMode::Sqlite => ready.sqlite.is_some(),
            };
            if allowed {
                ready.mode = next;
            }
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

fn format_rows_label(count: u64, cx: &App) -> String {
    t_fmt(
        cx,
        "{count, plural, one {{formatted} row} other {{formatted} rows}}",
        &[
            ("count", IcuArg::Int(i64::try_from(count).unwrap_or(i64::MAX))),
            ("formatted", IcuArg::Text(Cow::Owned(group_thousands(count)))),
        ],
    )
}

fn format_columns_label(count: usize, cx: &App) -> String {
    t_fmt(
        cx,
        "{count, plural, one {{formatted} column} other {{formatted} columns}}",
        &[
            ("count", IcuArg::Int(i64::try_from(count).unwrap_or(i64::MAX))),
            ("formatted", IcuArg::Text(Cow::Owned(group_thousands(count as u64)))),
        ],
    )
}

fn format_entries_label(count: usize, cx: &App) -> String {
    t_fmt(
        cx,
        "{count, plural, one {{formatted} entry} other {{formatted} entries}}",
        &[
            ("count", IcuArg::Int(i64::try_from(count).unwrap_or(i64::MAX))),
            ("formatted", IcuArg::Text(Cow::Owned(group_thousands(count as u64)))),
        ],
    )
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
                    sqlite,
                    mode,
                    folder,
                    selected_entry,
                    hex_scroll,
                    archive_scroll,
                    entry_scroll,
                } = ready.as_ref();
                let has_archive = archive.is_some();
                let has_sqlite = sqlite.is_some();
                let mode = *mode;
                let signature = hex.signature;
                let total_bytes = hex.total_bytes;
                let truncated = hex.truncated;
                let row_count = hex.row_count();
                let sqlite_header = hex.sqlite.clone();
                let sqlite_table_count = match sqlite {
                    Some(SqliteData::Ready(viewer)) => Some(viewer.db.tables().len()),
                    _ => None,
                };
                let folder_str = folder.clone();
                let entry_count = archive.as_ref().map_or(0, |a| a.entries().len());
                let hex_scroll = hex_scroll.clone();
                let archive_scroll = archive_scroll.clone();
                let entry_scroll = entry_scroll.clone();
                let selected_entry_info = selected_entry.as_ref().map(|(path, res)| (path.clone(), res.clone()));
                let children_len = archive
                    .as_ref()
                    .map_or(0, |a| archive_children(a.entries(), &folder_str).len());

                let summary_text = if let Some(table_count) = sqlite_table_count {
                    let table_unit = if table_count == 1 { "table" } else { "tables" };
                    format!(
                        "{} {table_unit} · {} bytes",
                        group_thousands(table_count as u64),
                        group_thousands(total_bytes)
                    )
                } else if has_archive {
                    format!(
                        "{} · {} bytes",
                        format_entries_label(entry_count, cx),
                        group_thousands(total_bytes)
                    )
                } else {
                    format!("{} bytes", group_thousands(total_bytes))
                };

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
                            .child(summary_text),
                    )
                    .when(has_sqlite, |this| {
                        this.child(
                            Button::new("binary-mode-sqlite")
                                .variant(Variant::Toggle)
                                .size(Size::Xs)
                                .label(t(cx, "Database"))
                                .pressed(mode == BinaryViewMode::Sqlite)
                                .on_click({
                                    let entity = entity.clone();
                                    move |_, _, cx| {
                                        if let Some(entity) = entity.upgrade() {
                                            entity.update(cx, |this, cx| {
                                                this.set_binary_mode(BinaryViewMode::Sqlite, cx);
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
                    })
                    .when(!has_sqlite && has_archive, |this| {
                        this.child(
                            Button::new("binary-mode-archive")
                                .variant(Variant::Toggle)
                                .size(Size::Xs)
                                .label(t(cx, "Archive"))
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
                    BinaryViewMode::Sqlite => self.render_sqlite_body(cx),
                    BinaryViewMode::Hex => v_flex()
                        .id("file-binary-hex")
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .bg(hsla(p.code_background))
                        .when_some(sqlite_header, |this, info| {
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
                                    .tooltip(t(
                                        cx,
                                        if selected_entry_info.is_some() {
                                            "Back to archive"
                                        } else {
                                            "Parent folder"
                                        },
                                    ))
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
                                                    t(cx, "Archive contents").to_owned()
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

    fn render_sqlite_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(BinaryState::Ready(ready)) = self.binary_state() else {
            return div().into_any_element();
        };
        let Some(sqlite) = ready.sqlite.as_ref() else {
            return div().into_any_element();
        };
        let viewer = match sqlite {
            SqliteData::Failed(error) => {
                let localized = t(cx, error).to_owned();
                return v_flex()
                    .id("file-sqlite-error")
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .px(px(24.))
                    .text_center()
                    .text_xs()
                    .text_color(hsla(p.destructive))
                    .child(localized)
                    .into_any_element();
            }
            SqliteData::Ready(viewer) => viewer.as_ref(),
        };

        let tables = viewer.db.tables().to_vec();
        if tables.is_empty() {
            return v_flex()
                .id("file-sqlite-empty")
                .flex_1()
                .items_center()
                .justify_center()
                .px(px(24.))
                .text_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(t(cx, "This database has no readable tables.").to_owned())
                .into_any_element();
        }

        let selected_name = viewer.selected_table.clone().unwrap_or_else(|| tables[0].name.clone());
        let active_table = viewer
            .active_table()
            .cloned()
            .unwrap_or_else(|| tables[0].clone());
        let range = sqlite_page_range(active_table.row_count, viewer.page, SQLITE_PAGE_SIZE);
        let busy = viewer.is_busy();
        let h_offset = viewer.h_offset;
        let rows_scroll = viewer.rows_scroll.clone();
        let tables_scroll = viewer.tables_scroll.clone();
        let viewport_width = viewer.viewport_width.clone();
        let column_widths = viewer.column_widths.clone();
        let entity = cx.entity().downgrade();

        let (page_columns, page_row_count, page_error, page_loading) = match &viewer.page_state {
            SqlitePageState::Loading => (None, 0, None, true),
            SqlitePageState::Failed(err) => (None, 0, Some(t(cx, err).to_owned()), false),
            SqlitePageState::Ready(data) => (Some(data.columns.clone()), data.rows.len(), None, false),
        };

        let table_tabs = tables.iter().enumerate().map(|(index, item)| {
            let is_selected = item.name == selected_name;
            let table_name = item.name.clone();
            let count_text = group_thousands(item.row_count);
            let is_view = item.kind == SqliteObjectKind::View;
            h_flex()
                .id(("sqlite-table-tab", index))
                .flex_none()
                .h(px(26.))
                .px(px(8.))
                .gap(px(5.))
                .items_center()
                .rounded(px(6.))
                .text_xs()
                .cursor_pointer()
                .when(is_selected, |this| {
                    this.bg(hsla(p.accent))
                        .text_color(hsla(p.foreground))
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!is_selected, |this| {
                    this.text_color(hsla(p.muted_foreground))
                        .hover(|s| s.bg(hsla(p.accent).opacity(0.6)))
                })
                .on_click({
                    let entity = entity.clone();
                    move |_, _, cx| {
                        if let Some(entity) = entity.upgrade() {
                            entity.update(cx, |this, cx| {
                                this.sqlite_select_table(table_name.clone(), cx);
                            });
                        }
                    }
                })
                .child(SharedString::from(item.name.clone()))
                .when(is_view, |this| {
                    this.child(
                        div()
                            .px(px(4.))
                            .rounded(px(3.))
                            .bg(hsla(p.secondary))
                            .text_size(px(10.))
                            .text_color(hsla(p.muted_foreground))
                            .child("view"),
                    )
                })
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(hsla(p.muted_foreground))
                        .child(format!("({count_text})")),
                )
        });

        let table_bar = h_flex()
            .id("sqlite-table-tabs")
            .flex_none()
            .h(px(36.))
            .px(px(8.))
            .gap(px(4.))
            .items_center()
            .overflow_x_scroll()
            .track_scroll(&tables_scroll)
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.6))
            .bg(hsla(p.background))
            .children(table_tabs);

        let rows_label = format_rows_label(active_table.row_count, cx);
        let cols_label = match &page_columns {
            Some(cols) => format_columns_label(cols.len(), cx),
            None => t(cx, "… columns").to_owned(),
        };
        let range_label = if range.total_rows == 0 {
            t(cx, "0 rows").to_owned()
        } else {
            t_fmt(
                cx,
                "{start}–{end} of {total}",
                &[
                    ("start", IcuArg::Text(Cow::Owned(group_thousands(range.start_row)))),
                    ("end", IcuArg::Text(Cow::Owned(group_thousands(range.end_row)))),
                    ("total", IcuArg::Text(Cow::Owned(group_thousands(range.total_rows)))),
                ],
            )
        };

        let prev_page = range.page.saturating_sub(1);
        let next_page = range.page.saturating_add(1);
        let pager_bar = h_flex()
            .flex_none()
            .min_h(px(34.))
            .px(px(12.))
            .py(px(4.))
            .gap(px(8.))
            .items_center()
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.6))
            .bg(hsla(p.background))
            .text_xs()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(hsla(p.muted_foreground))
                    .child(format!("{rows_label} · {cols_label}")),
            )
            .child(
                Button::new("sqlite-prev-page")
                    .variant(Variant::Ghost)
                    .size(Size::IconXs)
                    .icon(Icon::ChevronLeft)
                    .disabled(busy || !range.has_prev)
                    .tooltip(t(cx, "Previous rows"))
                    .on_click({
                        let entity = entity.clone();
                        move |_, _, cx| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(cx, |this, cx| this.sqlite_select_page(prev_page, cx));
                            }
                        }
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(hsla(p.foreground))
                    .child(SharedString::from(range_label)),
            )
            .child(
                Button::new("sqlite-next-page")
                    .variant(Variant::Ghost)
                    .size(Size::IconXs)
                    .icon(Icon::ChevronRight)
                    .disabled(busy || !range.has_next)
                    .tooltip(t(cx, "Next rows"))
                    .on_click({
                        let entity = entity.clone();
                        move |_, _, cx| {
                            if let Some(entity) = entity.upgrade() {
                                entity.update(cx, |this, cx| this.sqlite_select_page(next_page, cx));
                            }
                        }
                    }),
            );

        let notice = div()
            .flex_none()
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.5))
            .bg(hsla(p.muted).opacity(0.45))
            .px(px(12.))
            .py(px(4.))
            .text_size(px(11.))
            .text_color(hsla(p.muted_foreground))
            .child(
                t(
                    cx,
                    "Read-only. Up to 50 columns and 1,024 characters per cell. Blob contents are omitted.",
                )
                .to_owned(),
            );

        let grid: AnyElement = if page_loading {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hsla(p.muted_foreground))
                .child(t(cx, "Loading…").to_owned())
                .into_any_element()
        } else if let Some(err) = page_error {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .px(px(24.))
                .text_center()
                .text_xs()
                .text_color(hsla(p.destructive))
                .child(err)
                .into_any_element()
        } else if let Some(columns) = page_columns {
            let header_cells = columns.iter().enumerate().map(|(col_idx, col)| {
                let width = column_widths.get(col_idx).copied().unwrap_or(140.);
                h_flex()
                    .flex_none()
                    .w(px(width))
                    .h(px(SQLITE_HEADER_HEIGHT))
                    .px(px(10.))
                    .gap(px(4.))
                    .items_center()
                    .border_r_1()
                    .border_color(hsla(p.border).opacity(0.5))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(hsla(p.foreground))
                            .child(SharedString::from(col.name.clone())),
                    )
                    .when(col.is_primary_key, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .text_size(px(10.))
                                .text_color(hsla(p.primary))
                                .child("PK"),
                        )
                    })
                    .when(col.is_generated, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .text_size(px(10.))
                                .text_color(hsla(p.muted_foreground))
                                .child("GEN"),
                        )
                    })
            });

            let header_row = h_flex()
                .flex_none()
                .h(px(SQLITE_HEADER_HEIGHT))
                .border_b_1()
                .border_color(hsla(p.border))
                .bg(hsla(p.muted))
                .text_xs()
                .overflow_hidden()
                .child(
                    div()
                        .flex_none()
                        .w(px(SQLITE_GUTTER_WIDTH))
                        .h_full()
                        .border_r_1()
                        .border_color(hsla(p.border))
                        .bg(hsla(p.background))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .justify_end()
                        .text_color(hsla(p.muted_foreground))
                        .child("#"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(h_flex().ml(-h_offset).children(header_cells)),
                );

            let rows_body: AnyElement = if page_row_count == 0 {
                div()
                    .flex_1()
                    .p(px(24.))
                    .text_xs()
                    .text_color(hsla(p.muted_foreground))
                    .child(t(cx, "This table is empty.").to_owned())
                    .into_any_element()
            } else {
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(
                        uniform_list(
                            "sqlite-table-rows",
                            page_row_count,
                            cx.processor(move |this, vis: std::ops::Range<usize>, _, cx| {
                                vis.map(|index| this.render_sqlite_row(index, range.start_row, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .size_full()
                        .track_scroll(&rows_scroll),
                    )
                    .vertical_scrollbar(&rows_scroll)
                    .child(
                        canvas(
                            move |bounds, _, _| viewport_width.set(f32::from(bounds.size.width)),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .into_any_element()
            };

            v_flex()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .on_scroll_wheel(cx.listener(Self::scroll_sqlite_horizontally))
                .child(header_row)
                .child(rows_body)
                .into_any_element()
        } else {
            div().into_any_element()
        };

        v_flex()
            .id("file-binary-sqlite")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(hsla(p.background))
            .child(table_bar)
            .child(pager_bar)
            .child(notice)
            .child(grid)
            .into_any_element()
    }

    fn render_sqlite_row(&self, row_index: usize, start_row: u64, cx: &App) -> AnyElement {
        let p = palette(cx);
        let mono = cx.theme().mono_font_family.clone();
        let Some(BinaryState::Ready(ready)) = self.binary_state() else {
            return div().into_any_element();
        };
        let Some(SqliteData::Ready(viewer)) = ready.sqlite.as_ref() else {
            return div().into_any_element();
        };
        let SqlitePageState::Ready(data) = &viewer.page_state else {
            return div().into_any_element();
        };
        let Some(cells) = data.rows.get(row_index) else {
            return div().into_any_element();
        };
        let h_offset = viewer.h_offset;
        let striped = row_index % 2 == 1;
        let row_num = start_row.saturating_add(row_index as u64);

        h_flex()
            .h(px(SQLITE_ROW_HEIGHT))
            .w_full()
            .border_b_1()
            .border_color(hsla(p.border).opacity(0.45))
            .when(striped, |this| this.bg(hsla(p.muted).opacity(0.35)))
            .text_xs()
            .overflow_hidden()
            .child(
                div()
                    .flex_none()
                    .w(px(SQLITE_GUTTER_WIDTH))
                    .h_full()
                    .border_r_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .bg(hsla(p.background))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .justify_end()
                    .font_family(mono.clone())
                    .text_color(hsla(p.muted_foreground))
                    .child(SharedString::from(row_num.to_string())),
            )
            .child(
                div().flex_1().min_w_0().overflow_hidden().child(
                    h_flex().ml(-h_offset).children(
                        viewer.column_widths.iter().enumerate().map(|(col_idx, &width)| {
                            let cell = cells.get(col_idx);
                            let kind = cell.map_or(SqliteCellKind::Null, |c| c.kind);
                            let display = cell
                                .map(|c| c.display.replace('\n', " ↵ ").replace('\r', ""))
                                .unwrap_or_default();
                            let color = match kind {
                                SqliteCellKind::Null | SqliteCellKind::Blob => hsla(p.muted_foreground),
                                SqliteCellKind::Integer | SqliteCellKind::Real | SqliteCellKind::Text => {
                                    hsla(p.foreground)
                                }
                            };
                            let use_mono = matches!(
                                kind,
                                SqliteCellKind::Integer | SqliteCellKind::Real | SqliteCellKind::Blob
                            );
                            div()
                                .flex_none()
                                .w(px(width))
                                .h(px(SQLITE_ROW_HEIGHT))
                                .px(px(10.))
                                .border_r_1()
                                .border_color(hsla(p.border).opacity(0.35))
                                .flex()
                                .items_center()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .when(use_mono, |this| this.font_family(mono.clone()))
                                        .when(kind == SqliteCellKind::Null, |this| this.italic())
                                        .text_color(color)
                                        .child(SharedString::from(display)),
                                )
                        }),
                    ),
                ),
            )
            .into_any_element()
    }

    pub(super) fn scroll_sqlite_horizontally(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(px(SQLITE_ROW_HEIGHT));
        let dx = if event.modifiers.shift && delta.x == px(0.) {
            delta.y
        } else {
            delta.x
        };
        if !event.modifiers.shift && dx.abs() <= delta.y.abs() {
            return;
        }
        let Some(BinaryState::Ready(ready)) = self.binary_state_mut() else {
            return;
        };
        let Some(SqliteData::Ready(viewer)) = ready.sqlite.as_mut() else {
            return;
        };
        let total = viewer.total_grid_width();
        let visible = viewer.viewport_width.get().max(120.);
        let max = px((total - visible).max(0.));
        let next = (viewer.h_offset - dx).clamp(px(0.), max);
        if next != viewer.h_offset {
            viewer.h_offset = next;
            cx.notify();
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
