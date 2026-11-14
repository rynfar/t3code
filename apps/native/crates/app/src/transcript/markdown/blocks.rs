//! Custom GPUI `TextView` block extensions (`ALERT_NODE`, `DETAILS_NODE`, `MATH_NODE`,
//! `MERMAID_NODE`, `TABLE_NODE`) and fenced code block metadata lookup (`title`, closed state).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Entity, FontWeight, InteractiveElement, IntoElement, ObjectFit, ParentElement, RenderImage, SharedString,
    StatefulInteractiveElement, Styled, StyledImage, Window, div, img, px,
};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, MarkdownParseContext, TextView, TextViewState, markdown_ast as mdast};
use gpui_component::{ActiveTheme, h_flex, v_flex};
pub use t3_core::markdown::code::CodeFenceMetaMap;
use t3_core::markdown::alerts_details::{AlertKind, DetailsBlock, GithubAlert, parse_details_fence, parse_github_alert};
use t3_core::markdown::code::{is_code_fence_closed, is_math_language, is_mermaid_language};
use t3_core::markdown::math::render_tex_unicode;
use t3_core::markdown::mermaid::render_mermaid_svg;
use t3_core::markdown::table::TableData;

use super::links::MarkdownActionContext;
use crate::icons::Icon;
use crate::selectable_text::SelectableText;
use crate::theme::{hsla, palette};
use crate::toast::{Toast, ToastKind};
use crate::transcript::parts;

pub const ALERT_NODE: &str = "t3-alert";
pub const DETAILS_NODE: &str = "t3-details-block";
pub const MATH_NODE: &str = "t3-math-block";
pub const MERMAID_NODE: &str = "t3-mermaid-block";
pub const TABLE_NODE: &str = "t3-table-block";

pub type NestedViews = Arc<Mutex<HashMap<String, Entity<TextViewState>>>>;
pub type SharedActionContext = Arc<Mutex<Option<MarkdownActionContext>>>;
pub type SharedCodeMeta = Arc<Mutex<CodeFenceMetaMap>>;

#[derive(Clone)]
struct AlertNodeData {
    key: String,
    alert: GithubAlert,
}

#[derive(Clone)]
struct DetailsNodeData {
    key: String,
    details: DetailsBlock,
}

#[derive(Clone)]
struct MathNodeData {
    key: String,
    tex: String,
    display: String,
}

#[derive(Clone)]
struct MermaidNodeData {
    key: String,
    code: String,
}

#[derive(Clone)]
struct TableNodeData {
    key: String,
    data: TableData,
    raw_markdown: String,
}

struct ChildTextState {
    last_body: String,
    entity: Entity<TextViewState>,
}

struct DetailsUiState {
    open: bool,
    last_body: String,
    entity: Entity<TextViewState>,
}

struct MathUiState {
    show_tex: bool,
}

struct MermaidUiState {
    show_code: bool,
    dark: bool,
    source: String,
    svg: String,
    image: Option<Arc<RenderImage>>,
    intrinsic_width: f32,
    error: Option<String>,
}

/// Attaches custom rich-markdown parsers and renderers onto `base` (preserving existing media hooks).
pub fn attach_rich_extensions(
    base: MarkdownExtensions,
    action_ctx: SharedActionContext,
    nested_views: NestedViews,
    code_meta: SharedCodeMeta,
) -> MarkdownExtensions {
    attach_extensions_inner(base, action_ctx, nested_views, code_meta, true)
}

fn attach_extensions_inner(
    base: MarkdownExtensions,
    action_ctx: SharedActionContext,
    nested_views: NestedViews,
    code_meta: SharedCodeMeta,
    intercept_tables: bool,
) -> MarkdownExtensions {
    let ctx_for_child = action_ctx.clone();
    let nested_for_child = nested_views.clone();
    let meta_for_child = code_meta.clone();

    let with_parsers = base.block_parser(move |node, context| parse_rich_block(node, context, intercept_tables));

    let ctx_alert = ctx_for_child.clone();
    let nested_alert = nested_for_child.clone();
    let meta_alert = meta_for_child.clone();

    let ctx_details = ctx_for_child.clone();
    let nested_details = nested_for_child.clone();
    let meta_details = meta_for_child.clone();

    let ctx_mermaid = ctx_for_child.clone();

    let ctx_table = ctx_for_child;
    let nested_table = nested_for_child;
    let meta_table = meta_for_child;

    with_parsers
        .block_renderer(ALERT_NODE, move |node, window, cx| {
            let Some(data) = node.data::<AlertNodeData>() else { return div().into_any_element() };
            render_alert_block(data, &ctx_alert, &nested_alert, &meta_alert, window, cx)
        })
        .block_renderer(DETAILS_NODE, move |node, window, cx| {
            let Some(data) = node.data::<DetailsNodeData>() else { return div().into_any_element() };
            render_details_block(data, &ctx_details, &nested_details, &meta_details, window, cx)
        })
        .block_renderer(MATH_NODE, |node, window, cx| {
            let Some(data) = node.data::<MathNodeData>() else { return div().into_any_element() };
            render_math_block(data, window, cx)
        })
        .block_renderer(MERMAID_NODE, move |node, window, cx| {
            let Some(data) = node.data::<MermaidNodeData>() else { return div().into_any_element() };
            render_mermaid_block(data, &ctx_mermaid, window, cx)
        })
        .block_renderer(TABLE_NODE, move |node, window, cx| {
            let Some(data) = node.data::<TableNodeData>() else { return div().into_any_element() };
            render_table_block(data, &ctx_table, &nested_table, &meta_table, window, cx)
        })
}

fn parse_rich_block(node: &mdast::Node, context: &MarkdownParseContext<'_>, intercept_tables: bool) -> Option<MarkdownNode> {
    let key = node_key(node);
    match node {
        mdast::Node::Blockquote(bq) => {
            let alert = parse_github_alert(bq, context.source())?;
            let raw = context.node_source(node).unwrap_or(&alert.body_markdown).to_owned();
            let plain = if alert.body_markdown.is_empty() {
                alert.title.to_owned()
            } else {
                format!("{}\n{}", alert.title, alert.body_markdown)
            };
            Some(
                MarkdownNode::new(ALERT_NODE, AlertNodeData { key, alert })
                    .text(plain)
                    .markdown(raw),
            )
        }
        mdast::Node::Table(tbl) if intercept_tables => {
            let data = TableData::from_mdast(tbl);
            let raw = context
                .node_source(node)
                .map(str::to_owned)
                .unwrap_or_else(|| data.to_markdown());
            let plain = data
                .rows
                .iter()
                .map(|r| r.join("\t"))
                .collect::<Vec<_>>()
                .join("\n");
            Some(
                MarkdownNode::new(TABLE_NODE, TableNodeData { key, data, raw_markdown: raw.clone() })
                    .text(plain)
                    .markdown(raw),
            )
        }
        mdast::Node::Code(code) => {
            let lang = code.lang.as_deref();
            if lang == Some("t3-details")
                && let Some(details) = parse_details_fence(lang, code.meta.as_deref(), &code.value)
            {
                let plain = format!("{}\n{}", details.summary, details.body_markdown);
                let raw = context.node_source(node).unwrap_or(&code.value).to_owned();
                return Some(
                    MarkdownNode::new(DETAILS_NODE, DetailsNodeData { key, details })
                        .text(plain)
                        .markdown(raw),
                );
            }
            let closed = node
                .position()
                .is_some_and(|p| is_code_fence_closed(context.source(), p.start.offset));
            if lang == Some("t3-math") || (closed && is_math_language(lang)) {
                let display = render_tex_unicode(&code.value);
                let raw = context.node_source(node).unwrap_or(&code.value).to_owned();
                return Some(
                    MarkdownNode::new(MATH_NODE, MathNodeData { key, tex: code.value.clone(), display: display.clone() })
                        .text(display)
                        .markdown(raw),
                );
            }
            if closed && is_mermaid_language(lang) {
                let raw = context.node_source(node).unwrap_or(&code.value).to_owned();
                return Some(
                    MarkdownNode::new(MERMAID_NODE, MermaidNodeData { key, code: code.value.clone() })
                        .text(code.value.clone())
                        .markdown(raw),
                );
            }
            None
        }
        _ => None,
    }
}

fn node_key(node: &mdast::Node) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match node {
        mdast::Node::Blockquote(b) => {
            "bq".hash(&mut hasher);
            b.position.as_ref().map(|p| p.start.offset).hash(&mut hasher);
        }
        mdast::Node::Table(t) => {
            "tbl".hash(&mut hasher);
            t.position.as_ref().map(|p| p.start.offset).hash(&mut hasher);
            t.children.len().hash(&mut hasher);
        }
        mdast::Node::Code(c) => {
            "code".hash(&mut hasher);
            c.lang.hash(&mut hasher);
            c.meta.hash(&mut hasher);
            c.value.hash(&mut hasher);
        }
        _ => {}
    }
    format!("md-block-{:x}", hasher.finish())
}

fn render_alert_block(
    data: &AlertNodeData,
    action_ctx: &SharedActionContext,
    nested_views: &NestedViews,
    code_meta: &SharedCodeMeta,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let p = palette(cx);
    let (accent, icon) = match data.alert.kind {
        AlertKind::Note => (p.info, Icon::Info),
        AlertKind::Tip => (p.success, Icon::Sparkles),
        AlertKind::Important => (p.primary, Icon::CircleAlert),
        AlertKind::Warning => (p.warning, Icon::TriangleAlert),
        AlertKind::Caution => (p.destructive, Icon::CircleX),
    };

    let body = data.alert.body_markdown.clone();
    let key = SharedString::from(format!("alert-body:{}", data.key));
    let state = window.use_keyed_state(key.clone(), cx, |_, cx| ChildTextState {
        last_body: body.clone(),
        entity: cx.new(|cx| TextViewState::markdown(&body, cx).selectable(true)),
    });
    let entity = state.update(cx, |s, cx| {
        if s.last_body != body {
            s.last_body = body.clone();
            s.entity.update(cx, |tv, cx| tv.set_text(&body, cx));
        }
        s.entity.clone()
    });
    nested_views.lock().unwrap().insert(data.key.clone(), entity.clone());

    let child_ext = attach_extensions_inner(MarkdownExtensions::default(), action_ctx.clone(), nested_views.clone(), code_meta.clone(), true);
    let view = super::style_markdown_internal(
        TextView::new(&entity).markdown_extensions(child_ext),
        action_ctx.lock().unwrap().clone(),
        code_meta.clone(),
        cx,
    );

    v_flex()
        .w_full()
        .min_w_0()
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(accent).opacity(0.32))
        .border_l_4()
        .bg(hsla(accent).opacity(0.08))
        .px(px(12.))
        .py(px(10.))
        .gap(px(6.))
        .child(
            h_flex()
                .gap(px(6.))
                .items_center()
                .child(icon.render(px(14.), hsla(accent)))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(hsla(accent))
                        .child(data.alert.title),
                ),
        )
        .when(!data.alert.body_markdown.is_empty(), |this| this.child(view))
        .into_any_element()
}

fn render_details_block(
    data: &DetailsNodeData,
    action_ctx: &SharedActionContext,
    nested_views: &NestedViews,
    code_meta: &SharedCodeMeta,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let p = palette(cx);
    let body = data.details.body_markdown.clone();
    let default_open = data.details.open;
    let key = SharedString::from(format!("details:{}", data.key));
    let state = window.use_keyed_state(key.clone(), cx, |_, cx| DetailsUiState {
        open: default_open,
        last_body: body.clone(),
        entity: cx.new(|cx| TextViewState::markdown(&body, cx).selectable(true)),
    });
    let (open, entity) = state.update(cx, |s, cx| {
        if s.last_body != body {
            s.last_body = body.clone();
            s.entity.update(cx, |tv, cx| tv.set_text(&body, cx));
        }
        (s.open, s.entity.clone())
    });
    if open {
        nested_views.lock().unwrap().insert(data.key.clone(), entity.clone());
    }

    let child_ext = attach_extensions_inner(MarkdownExtensions::default(), action_ctx.clone(), nested_views.clone(), code_meta.clone(), true);
    let body_view = super::style_markdown_internal(
        TextView::new(&entity).markdown_extensions(child_ext),
        action_ctx.lock().unwrap().clone(),
        code_meta.clone(),
        cx,
    );

    let toggle_state = state.clone();
    v_flex()
        .w_full()
        .min_w_0()
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.border))
        .bg(hsla(p.card).opacity(0.45))
        .overflow_hidden()
        .child(
            h_flex()
                .id(SharedString::from(format!("details-toggle:{}", data.key)))
                .w_full()
                .gap(px(8.))
                .items_center()
                .px(px(12.))
                .py(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(hsla(p.accent).opacity(0.08)))
                .on_click(move |_, _, cx| {
                    toggle_state.update(cx, |s, cx| {
                        s.open = !s.open;
                        cx.notify();
                    });
                })
                .child(
                    if open { Icon::ChevronDown } else { Icon::ChevronRight }
                        .render(px(14.), hsla(p.muted_foreground)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hsla(p.foreground))
                        .child(data.details.summary.clone()),
                ),
        )
        .when(open && !data.details.body_markdown.is_empty(), |this| {
            this.child(
                div()
                    .w_full()
                    .border_t_1()
                    .border_color(hsla(p.border).opacity(0.7))
                    .px(px(12.))
                    .py(px(10.))
                    .child(body_view),
            )
        })
        .into_any_element()
}

fn render_math_block(data: &MathNodeData, window: &mut Window, cx: &mut App) -> AnyElement {
    let p = palette(cx);
    let key = SharedString::from(format!("math:{}", data.key));
    let ui = window.use_keyed_state(key, cx, |_, _| MathUiState { show_tex: false });
    let show_tex = ui.read(cx).show_tex;

    let toggle_ui = ui.clone();
    let tex_for_copy = data.tex.clone();
    let shown_text = if show_tex { data.tex.clone() } else { data.display.clone() };
    let group_id = SharedString::from(format!("math-grp:{}", data.key));

    v_flex()
        .w_full()
        .min_w_0()
        .group(group_id.clone())
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.code_block_border))
        .bg(hsla(p.code_block_background))
        .px(px(14.))
        .py(px(10.))
        .gap(px(6.))
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_size(px(11.))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(hsla(p.muted_foreground))
                        .child(if show_tex { "TeX" } else { "Math" }),
                )
                .child(
                    h_flex()
                        .gap(px(4.))
                        .items_center()
                        .child(parts::ghost_text_button(
                            SharedString::from(format!("math-toggle:{}", data.key)),
                            if show_tex { "Rendered" } else { "TeX" },
                            None,
                            move |_, _, cx| {
                                toggle_ui.update(cx, |s, cx| {
                                    s.show_tex = !s.show_tex;
                                    cx.notify();
                                });
                            },
                            cx,
                        ))
                        .child(parts::ghost_icon_button(
                            SharedString::from(format!("math-copy:{}", data.key)),
                            Icon::Copy,
                            move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(tex_for_copy.clone())),
                            cx,
                        )),
                ),
        )
        .child(
            div()
                .id(SharedString::from(format!("math-scroll:{}", data.key)))
                .w_full()
                .overflow_x_scroll()
                .py(px(4.))
                .child(
                    div()
                        .when(!show_tex, |d| d.flex().justify_center().text_size(px(16.)).line_height(px(24.)))
                        .when(show_tex, |d| {
                            d.font_family(cx.theme().mono_font_family.clone())
                                .text_size(cx.theme().mono_font_size)
                        })
                        .text_color(hsla(p.foreground))
                        .child(SelectableText::new(SharedString::from(format!("math-sel:{}", data.key)), shown_text)),
                ),
        )
        .into_any_element()
}

fn render_mermaid_block(
    data: &MermaidNodeData,
    action_ctx: &SharedActionContext,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let p = palette(cx);
    let is_dark = cx.theme().mode.is_dark();
    let key = SharedString::from(format!("mermaid:{}", data.key));
    let svg_renderer = cx.svg_renderer();

    let state = window.use_keyed_state(key, cx, |_, _| MermaidUiState {
        show_code: false,
        dark: !is_dark,
        source: String::new(),
        svg: String::new(),
        image: None,
        intrinsic_width: 0.0,
        error: None,
    });

    let (show_code, image, svg_text, error, intrinsic_width) = state.update(cx, |s, _| {
        if s.source != data.code || s.dark != is_dark {
            s.source = data.code.clone();
            s.dark = is_dark;
            match render_mermaid_svg(&data.code, is_dark) {
                Ok(rendered) => {
                    s.intrinsic_width = rendered.width;
                    s.svg = rendered.svg;
                    match svg_renderer.render_single_frame(s.svg.as_bytes(), 2.0) {
                        Ok(img_frame) => {
                            s.image = Some(img_frame);
                            s.error = None;
                        }
                        Err(err) => {
                            s.image = None;
                            s.error = Some(format!("SVG render failed: {err}"));
                        }
                    }
                }
                Err(err) => {
                    s.image = None;
                    s.svg.clear();
                    s.error = Some(err);
                }
            }
        }
        (s.show_code, s.image.clone(), s.svg.clone(), s.error.clone(), s.intrinsic_width)
    });

    let show_diagram = !show_code && error.is_none() && image.is_some();
    let toggle_state = state.clone();
    let copy_code = data.code.clone();
    let expand_ctx = action_ctx.lock().unwrap().clone();
    let expand_svg = svg_text.clone();

    v_flex()
        .w_full()
        .min_w_0()
        .rounded(px(8.))
        .border_1()
        .border_color(hsla(p.code_block_border))
        .bg(hsla(p.code_block_background))
        .overflow_hidden()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .items_center()
                .px(px(12.))
                .py(px(6.))
                .border_b_1()
                .border_color(hsla(p.border).opacity(0.6))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .items_center()
                        .child(
                            div()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(11.))
                                .text_color(hsla(p.foreground).opacity(0.72))
                                .child("mermaid"),
                        )
                        .when_some(error.as_ref(), |this, _| {
                            this.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(hsla(p.muted_foreground))
                                    .child("Preview unavailable — showing code"),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .gap(px(4.))
                        .items_center()
                        .when(error.is_none() && image.is_some(), |this| {
                            this.child(parts::ghost_text_button(
                                SharedString::from(format!("mermaid-toggle:{}", data.key)),
                                if show_code { "Diagram" } else { "Code" },
                                Some(Icon::CodeXml),
                                move |_, _, cx| {
                                    toggle_state.update(cx, |s, cx| {
                                        s.show_code = !s.show_code;
                                        cx.notify();
                                    });
                                },
                                cx,
                            ))
                        })
                        .when(show_diagram && expand_ctx.as_ref().and_then(|c| c.thread_view.as_ref()).is_some(), |this| {
                            let ctx = expand_ctx.clone().unwrap();
                            let svg_for_expand = expand_svg.clone();
                            this.child(parts::ghost_icon_button(
                                SharedString::from(format!("mermaid-expand:{}", data.key)),
                                Icon::Maximize2,
                                move |_, window, cx| {
                                    if let Some(tv) = ctx.thread_view.as_ref().and_then(|w| w.upgrade()) {
                                        let data_url = svg_data_url(&svg_for_expand);
                                        tv.update(cx, |view, cx| {
                                            view.open_media(
                                                vec![crate::media::ViewerItem {
                                                    source: t3_core::media::MediaSource::Direct(data_url),
                                                    name: "Mermaid diagram".into(),
                                                }],
                                                0,
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                },
                                cx,
                            ))
                        })
                        .child(parts::ghost_icon_button(
                            SharedString::from(format!("mermaid-copy:{}", data.key)),
                            Icon::Copy,
                            move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_code.clone())),
                            cx,
                        )),
                ),
        )
        .child(if show_diagram {
            let rendered_img = image.unwrap();
            let display_w = intrinsic_width.max(240.0);
            div()
                .w_full()
                .p(px(12.))
                .flex()
                .justify_center()
                .child(
                    img(rendered_img)
                        .w(px(display_w))
                        .max_w_full()
                        .object_fit(ObjectFit::Contain),
                )
                .into_any_element()
        } else {
            div()
                .id(SharedString::from(format!("mermaid-code:{}", data.key)))
                .w_full()
                .overflow_x_scroll()
                .px(px(14.))
                .py(px(12.))
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(cx.theme().mono_font_size)
                .line_height(cx.theme().mono_font_size * 1.375)
                .text_color(hsla(p.foreground))
                .child(SelectableText::new(SharedString::from(format!("mermaid-sel:{}", data.key)), data.code.clone()))
                .into_any_element()
        })
        .into_any_element()
}

fn render_table_block(
    data: &TableNodeData,
    action_ctx: &SharedActionContext,
    nested_views: &NestedViews,
    code_meta: &SharedCodeMeta,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let p = palette(cx);
    let raw = data.raw_markdown.clone();
    let key = SharedString::from(format!("table:{}", data.key));
    let state = window.use_keyed_state(key, cx, |_, cx| ChildTextState {
        last_body: raw.clone(),
        entity: cx.new(|cx| TextViewState::markdown(&raw, cx).selectable(true)),
    });
    let entity = state.update(cx, |s, cx| {
        if s.last_body != raw {
            s.last_body = raw.clone();
            s.entity.update(cx, |tv, cx| tv.set_text(&raw, cx));
        }
        s.entity.clone()
    });
    nested_views.lock().unwrap().insert(data.key.clone(), entity.clone());

    let child_ext = attach_extensions_inner(MarkdownExtensions::default(), action_ctx.clone(), nested_views.clone(), code_meta.clone(), false);
    let table_view = super::style_markdown_internal(
        TextView::new(&entity).markdown_extensions(child_ext),
        action_ctx.lock().unwrap().clone(),
        code_meta.clone(),
        cx,
    );

    let md_copy = data.data.to_markdown();
    let csv_copy = data.data.to_csv();
    let ctx_md = action_ctx.lock().unwrap().as_ref().map(|c| c.state.clone());
    let ctx_csv = ctx_md.clone();

    v_flex()
        .w_full()
        .min_w_0()
        .gap(px(4.))
        .child(
            h_flex()
                .justify_end()
                .gap(px(4.))
                .child(parts::ghost_text_button(
                    SharedString::from(format!("table-copy-md:{}", data.key)),
                    "Copy Markdown",
                    Some(Icon::Copy),
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(md_copy.clone()));
                        if let Some(app) = &ctx_md {
                            app.update(cx, |a, cx| a.show_toast(Toast::new(ToastKind::Success, "Copied table as Markdown"), cx));
                        }
                    },
                    cx,
                ))
                .child(parts::ghost_text_button(
                    SharedString::from(format!("table-copy-csv:{}", data.key)),
                    "Copy CSV",
                    Some(Icon::Copy),
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(csv_copy.clone()));
                        if let Some(app) = &ctx_csv {
                            app.update(cx, |a, cx| a.show_toast(Toast::new(ToastKind::Success, "Copied table as CSV"), cx));
                        }
                    },
                    cx,
                )),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .border_color(hsla(p.border))
                .child(table_view),
        )
        .into_any_element()
}

fn svg_data_url(svg: &str) -> String {
    let mut out = String::from("data:image/svg+xml,");
    for &b in svg.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => {
                out.push('%');
                out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
                out.push(char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
            }
        }
    }
    out
}
