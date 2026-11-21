use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex};

use gpui::{App, AppContext, Context, Entity, HighlightStyle, IntoElement, ParentElement, Render, Styled, Window, div};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, MarkdownParseContext, TextView, TextViewState, markdown_ast::Node};
use t3_core::assistant_citations::{self as rules, Citation};

use super::markdown::blocks::{NestedSelection, NestedViews, SharedActionContext, SharedCodeMeta};

use crate::theme::palette;

#[derive(Clone, Copy)]
pub(crate) struct FindHighlights<'a> {
    pub query: &'a str,
    pub active: Option<(&'a str, u64)>,
}

#[derive(Clone, Default)]
pub(super) struct ToolFind(Option<Rc<ToolMatches>>);

struct ToolMatches {
    query: String,
    active: Option<u64>,
    next: Cell<u64>,
}

impl ToolFind {
    pub fn new(id: &str, find: Option<FindHighlights<'_>>) -> Self {
        Self(find.map(|find| Rc::new(ToolMatches {
            query: find.query.to_owned(),
            active: find.active.filter(|(entry, _)| *entry == id).map(|(_, index)| index),
            next: Cell::new(0),
        })))
    }

    pub fn styles(&self, text: &str, cx: &App) -> Vec<(Range<usize>, HighlightStyle)> {
        let Some(matches) = &self.0 else { return Vec::new() };
        let p = palette(cx);
        occurrences(text, &matches.query).into_iter().map(|range| {
            let index = matches.next.get();
            matches.next.set(index + 1);
            let color = if matches.active == Some(index) { p.search_match_active_background } else { p.search_match_background };
            (range, HighlightStyle { background_color: Some(crate::theme::hsla(color)), ..Default::default() })
        }).collect()
    }

    pub fn combine(&self, text: &str, syntax: &crate::selectable_text::Highlights, cx: &App) -> crate::selectable_text::Highlights {
        let marks = self.styles(text, cx);
        if marks.is_empty() {
            return syntax.clone();
        }
        gpui::combine_highlights(syntax.iter().cloned(), marks).collect()
    }
}

fn fold_character(character: char) -> char {
    let mut lowered = character.to_lowercase();
    let first = lowered.next().unwrap_or(character);
    match first {
        'ς' => 'σ',
        '‘' | '’' => '\'',
        _ if lowered.next().is_some() => character,
        _ => first,
    }
}

fn occurrences(text: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let query: String = query.chars().map(fold_character).collect();
    let mut folded = String::with_capacity(text.len());
    let mut offsets = Vec::new();
    for (offset, character) in text.char_indices() {
        offsets.push((folded.len(), offset));
        folded.push(fold_character(character));
    }
    offsets.push((folded.len(), text.len()));
    let original_offset = |index| {
        offsets
            .binary_search_by_key(&index, |&(folded, _)| folded)
            .ok()
            .map(|ix| offsets[ix].1)
    };
    folded
        .match_indices(&query)
        .filter_map(|(offset, matched)| {
            Some(original_offset(offset)?..original_offset(offset + matched.len())?)
        })
        .collect()
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct MarkdownMarks {
    query: Option<(String, Option<u64>)>,
    citation: Option<Citation>,
}

impl MarkdownMarks {
    pub(super) fn new(id: &str, find: Option<FindHighlights<'_>>, citation: Option<&Citation>) -> Option<Self> {
        let query = find.map(|find| (find.query.to_owned(), find.active
            .filter(|(entry, _)| *entry == id)
            .map(|(_, occurrence)| occurrence)));
        let citation = citation.filter(|citation| citation.message_id == id).cloned();
        (query.is_some() || citation.is_some()).then_some(Self { query, citation })
    }
}

#[derive(Clone)]
pub(crate) struct CitationPulse {
    pub citation: Citation,
    started: Instant,
}

impl CitationPulse {
    pub fn new(citation: Citation) -> Self {
        Self { citation, started: Instant::now() }
    }

    pub fn finished(&self) -> bool {
        self.started.elapsed() >= Duration::from_secs(3)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Find,
    ActiveFind,
    Citation,
}

#[derive(Default)]
struct Inline {
    text: String,
    runs: Vec<(Range<usize>, Tags)>,
}

#[derive(Clone, Default)]
struct Tags {
    formatting: Vec<&'static str>,
    link: Option<String>,
}

impl Inline {
    fn append(&mut self, node: &Node, mut tags: Tags) -> Option<()> {
        let value = match node {
            Node::Text(node) => node.value.as_str(),
            Node::InlineCode(node) => {
                tags.formatting.push("code");
                node.value.as_str()
            }
            Node::InlineMath(node) => {
                tags.formatting.push("code");
                node.value.as_str()
            }
            Node::Break(_) => "\n",
            Node::Strong(_) | Node::Emphasis(_) | Node::Delete(_) | Node::Link(_) => {
                match node {
                    Node::Strong(_) => tags.formatting.push("strong"),
                    Node::Emphasis(_) => tags.formatting.push("em"),
                    Node::Delete(_) => tags.formatting.push("del"),
                    Node::Link(link) => tags.link = Some(link.url.clone()),
                    _ => {}
                }
                for child in node.children()? {
                    self.append(child, tags.clone())?;
                }
                return Some(());
            }
            _ => return None,
        };
        let start = self.text.len();
        self.text.push_str(value);
        self.runs.push((start..self.text.len(), tags));
        Some(())
    }

    fn html(&self, level: Option<u8>, marks: &[(Range<usize>, Mark)], colors: [u32; 3]) -> String {
        let tag = level.map_or_else(|| "p".to_owned(), |level| format!("h{level}"));
        let mut out = format!("<{tag}>");
        for (run, tags) in &self.runs {
            if let Some(url) = &tags.link {
                out.push_str("<a href=\"");
                escape_html(&mut out, url);
                out.push_str("\">");
            }
            for tag in &tags.formatting {
                out.push_str(&format!("<{tag}>"));
            }
            let mut offset = run.start;
            let first = marks.partition_point(|(range, _)| range.end <= run.start);
            for (range, mark) in marks[first..].iter().take_while(|(range, _)| range.start < run.end) {
                let start = range.start.max(run.start);
                let end = range.end.min(run.end);
                let color = match mark {
                    Mark::Find => colors[0],
                    Mark::ActiveFind => colors[1],
                    Mark::Citation => colors[2],
                };
                escape_html(&mut out, &self.text[offset..start]);
                out.push_str(&format!(r##"<mark color="#{:08x}">"##, color));
                escape_html(&mut out, &self.text[start..end]);
                out.push_str("</mark>");
                offset = end;
            }
            escape_html(&mut out, &self.text[offset..run.end]);
            for tag in tags.formatting.iter().rev() {
                out.push_str(&format!("</{tag}>"));
            }
            if tags.link.is_some() {
                out.push_str("</a>");
            }
        }
        out.push_str(&format!("</{tag}>"));
        out
    }
}

fn escape_html(out: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(character),
        }
    }
}

const HIGHLIGHTED_PROSE: &str = "t3-highlighted-prose";

struct HighlightedProse {
    key: String,
    inline: Inline,
    level: Option<u8>,
    find: Vec<(Range<usize>, Mark)>,
    cited: Option<Range<usize>>,
}

fn parse_block(node: &Node, context: &MarkdownParseContext<'_>, marks: &MarkdownMarks) -> Option<MarkdownNode> {
    let level = match node {
        Node::Paragraph(_) => None,
        Node::Heading(heading) => Some(heading.depth),
        _ => return None,
    };
    let mut inline = Inline::default();
    for child in node.children()? {
        inline.append(child, Tags::default())?;
    }
    let position = node.position()?;
    let source = context.source();
    let current = context.node_source(node)?;
    let before = source.get(..position.start.offset)?;
    let find = marks.query.as_ref().map_or_else(Vec::new, |(query, active)| {
        // Find treats soft line breaks as spaces within a prose block.
        let ranges = occurrences(&inline.text.replace('\n', " "), query);
        let first = if active.is_some() && !ranges.is_empty() {
            rules::markdown_text(before).lines().map(|line| occurrences(line, query).len()).sum::<usize>()
        } else {
            0
        };
        ranges.into_iter().enumerate().map(|(index, range)| {
            let is_active = context.offset() == 0 && *active == Some((first + index) as u64);
            (range, if is_active { Mark::ActiveFind } else { Mark::Find })
        }).collect()
    });
    let cited = marks.citation.as_ref().and_then(|citation| {
        let through_block = rules::markdown_text(source.get(..position.end.offset)?);
        let start = through_block.strip_suffix(inline.text.trim_end())?.len();
        let resolved = rules::find_citation_range(&rules::markdown_text(source), citation)?.range;
        let overlap = resolved.start.max(start)..resolved.end.min(start + inline.text.len());
        (!overlap.is_empty()).then(|| overlap.start - start..overlap.end - start)
    });
    if find.is_empty() && cited.is_none() {
        return None;
    }
    let key = format!("marked-prose:{}", context.offset() + position.start.offset);
    let text = inline.text.clone();
    Some(MarkdownNode::new(HIGHLIGHTED_PROSE, Arc::new(HighlightedProse { key, inline, level, find, cited }))
        .text(text)
        .markdown(current.to_owned()))
}

pub(super) fn mark_extensions(
    extensions: MarkdownExtensions,
    marks: MarkdownMarks,
    pulse: Arc<Mutex<Option<CitationPulse>>>,
    action: SharedActionContext,
    code_meta: SharedCodeMeta,
    nested: NestedViews,
) -> MarkdownExtensions {
    extensions.block_parser(move |node, context| parse_block(node, context, &marks))
        .block_renderer(HIGHLIGHTED_PROSE, move |node, window, cx| {
            let Some(data) = node.data::<Arc<HighlightedProse>>() else { return div().into_any_element() };
            let child = html_state(data, None, window, cx);
            nested.lock().unwrap().insert(data.key.clone(), NestedSelection::Markdown(child.clone()));
            let ctx = action.lock().unwrap().clone();
            let pulse = pulse.lock().unwrap().clone();
            let overlay = if let (Some(range), Some(pulse)) = (&data.cited, pulse) {
                let cited = html_state(data, Some(range), window, cx);
                let key = gpui::ElementId::NamedInteger(data.key.clone().into(), 2);
                let view = window.use_keyed_state(key, cx, |_, _| PulseView::new(pulse.started, cited.clone()));
                view.update(cx, |view, _| {
                    if view.started != pulse.started || view.text != cited {
                        *view = PulseView::new(pulse.started, cited);
                    }
                });
                Some(div().absolute().inset_0().child(view))
            } else {
                None
            };
            div().min_w_0().relative()
                .child(super::markdown::style_markdown_internal(TextView::new(&child), ctx, code_meta.clone(), cx))
                .children(overlay)
                .into_any_element()
        })
}

struct HtmlState {
    data: Arc<HighlightedProse>,
    colors: [u32; 3],
    child: Entity<TextViewState>,
}

fn html_state(data: &Arc<HighlightedProse>, cited: Option<&Range<usize>>, window: &mut Window, cx: &mut App) -> Entity<TextViewState> {
    let p = palette(cx);
    let colors = [p.search_match_background, p.search_match_active_background, p.context_chip_citation];
    let key = gpui::ElementId::NamedInteger(data.key.clone().into(), u64::from(cited.is_some()));
    let html = || match cited {
        Some(range) => data.inline.html(data.level, &[(range.clone(), Mark::Citation)], colors),
        None => data.inline.html(data.level, &data.find, colors),
    };
    let entity = window.use_keyed_state(key, cx, |_, cx| HtmlState {
        data: data.clone(),
        colors,
        child: cx.new(|cx| TextViewState::html(&html(), cx).selectable(cited.is_none())),
    });
    entity.update(cx, |state, cx| {
        if !Arc::ptr_eq(&state.data, data) || state.colors != colors {
            let html = html();
            state.child.update(cx, |child, cx| child.set_text(&html, cx));
            state.data = data.clone();
            state.colors = colors;
        }
        state.child.clone()
    })
}

struct PulseView {
    started: Instant,
    text: Entity<TextViewState>,
    pending: Rc<Cell<bool>>,
}

impl PulseView {
    fn new(started: Instant, text: Entity<TextViewState>) -> Self {
        Self { started, text, pending: Rc::default() }
    }
}

impl Render for PulseView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced = crate::components::motion::prefers_reduced_motion(cx);
        let elapsed = self.started.elapsed().as_secs_f32() * 1000.;
        if elapsed >= 3000. {
            return div();
        }
        let stops = [(0., 0.), (325., 0.45), (650., 0.), (975., 0.45), (2550., 0.45), (3000., 0.)];
        let (opacity, delay) = if reduced {
            (0.45, 3000. - elapsed)
        } else {
            stops.windows(2).find_map(|pair| {
                let [(a, start), (b, end)] = pair else { return None };
                (elapsed <= *b).then(|| {
                    let phase = (elapsed - a) / (b - a);
                    let delay = if start == end { b - elapsed } else { 34. };
                    (start + (end - start) * phase, delay)
                })
            }).unwrap_or((0., 34.))
        };
        let pending = self.pending.clone();
        let owner = cx.weak_entity();
        div().relative().size_full().opacity(opacity)
            .child(gpui::canvas(|_, _, _| (), move |bounds, (), window, cx| {
                if !bounds.intersects(&window.content_mask().bounds) || pending.replace(true) {
                    return;
                }
                window.spawn(cx, async move |cx| {
                    cx.background_executor().timer(Duration::from_millis((delay.ceil() as u64).max(1))).await;
                    let _ = cx.update(|window, _| {
                        if owner.upgrade().is_none() {
                            return;
                        }
                        window.on_next_frame(move |_, cx| {
                            pending.set(false);
                            if let Some(owner) = owner.upgrade() {
                                owner.update(cx, |view, cx| {
                                    if Rc::ptr_eq(&pending, &view.pending) {
                                        cx.notify();
                                    }
                                });
                            }
                        });
                    });
                }).detach();
            }).absolute().inset_0())
            .child(super::markdown::style_markdown(TextView::new(&self.text), cx).selectable(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_preserve_original_utf8_offsets_after_case_and_apostrophe_folding() {
        let text = "İ ΟΣ ος οσ don’t don't";
        assert_eq!(
            occurrences(text, "Σ")
                .iter()
                .map(|range| &text[range.clone()])
                .collect::<Vec<_>>(),
            ["Σ", "ς", "σ"]
        );
        assert_eq!(
            occurrences(text, "don't")
                .iter()
                .map(|range| &text[range.clone()])
                .collect::<Vec<_>>(),
            ["don’t", "don't"]
        );
        assert_eq!(occurrences("aaaa", "aa"), [0..2, 2..4]);
    }

    #[test]
    fn a_match_spanning_formatted_runs_marks_only_the_visible_substrings() {
        let inline = Inline {
            text: "one blue bird flies".to_owned(),
            runs: vec![
                (0..4, Tags::default()),
                (4..8, Tags { formatting: vec!["strong"], link: None }),
                (8..19, Tags::default()),
            ],
        };
        let marks = occurrences(&inline.text, "BLUE BIRD").into_iter().map(|range| (range, Mark::ActiveFind)).collect::<Vec<_>>();
        let p = crate::theme::BUILT_IN_THEMES[0].light.unwrap();
        let html = inline.html(None, &marks, [p.search_match_background, p.search_match_active_background, p.context_chip_citation]);
        assert!(html.starts_with("<p>one <strong><mark "));
        assert!(html.contains(">blue</mark></strong><mark "));
        assert!(html.ends_with("> bird</mark> flies</p>"));
    }
}
