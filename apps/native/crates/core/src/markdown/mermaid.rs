//! Pure-Rust Mermaid parser and resvg-compatible SVG renderer for GPUI (`TR-MARKDOWN`).
//! Supports `flowchart` / `graph`, `sequenceDiagram`, `stateDiagram` (`-v2`), `pie`,
//! `classDiagram`, and `erDiagram`, returning `Err` on empty or unsupported syntax so the UI
//! cleanly falls back to code view (`apps/web/src/components/chat/MarkdownMermaidCodeBlock.tsx`).

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;

/// Theme-aware SVG palette matching GPUI dark/light surfaces.
#[derive(Debug, Clone, Copy)]
pub struct MermaidPalette {
    pub background: &'static str,
    pub surface: &'static str,
    pub alt_surface: &'static str,
    pub border: &'static str,
    pub edge: &'static str,
    pub text: &'static str,
    pub muted: &'static str,
    pub accent: &'static str,
    pub series: [&'static str; 6],
}

impl MermaidPalette {
    pub const fn for_theme(is_dark: bool) -> Self {
        if is_dark {
            Self {
                background: "#13151A",
                surface: "#1B1F27",
                alt_surface: "#222733",
                border: "#384152",
                edge: "#7E8BA3",
                text: "#E6EAF2",
                muted: "#9AA5B8",
                accent: "#6EA8FE",
                series: ["#6EA8FE", "#5FD1A5", "#F6B756", "#D98BF0", "#FF7B89", "#69D2E7"],
            }
        } else {
            Self {
                background: "#FAFBFC",
                surface: "#FFFFFF",
                alt_surface: "#F2F4F8",
                border: "#CBD5E1",
                edge: "#64748B",
                text: "#0F172A",
                muted: "#475569",
                accent: "#2563EB",
                series: ["#2563EB", "#0D9488", "#D97706", "#9333EA", "#E11D48", "#0284C7"],
            }
        }
    }
}

/// Rendered SVG output together with intrinsic dimensions for GPUI `svg` / `RenderImage` layout.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedMermaidSvg {
    pub svg: String,
    pub width: f32,
    pub height: f32,
}

/// Parses a Mermaid definition and renders an SVG document, or returns an error description when
/// the diagram uses an unsupported type or has no renderable elements.
pub fn render_mermaid_svg(source: &str, is_dark: bool) -> Result<RenderedMermaidSvg, String> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return Err("Empty Mermaid diagram.".into());
    }

    let palette = MermaidPalette::for_theme(is_dark);
    let lines = strip_directives_and_comments(trimmed);
    let first = lines
        .first()
        .ok_or_else(|| "Empty Mermaid diagram.".to_owned())?
        .trim();
    let lower = first.to_ascii_lowercase();

    if lower.starts_with("flowchart") || lower.starts_with("graph") {
        let dir = parse_flow_direction(first);
        render_flowchart(&lines[1..], dir, palette)
    } else if lower.starts_with("sequencediagram") {
        render_sequence(&lines[1..], palette)
    } else if lower.starts_with("statediagram") {
        render_state_diagram(&lines[1..], palette)
    } else if lower.starts_with("pie") {
        render_pie(first, &lines[1..], palette)
    } else if lower.starts_with("classdiagram") || lower.starts_with("erdiagram") {
        render_box_diagram(&lines[1..], palette)
    } else {
        Err(format!("Unsupported Mermaid diagram type: {}", first.split_whitespace().next().unwrap_or("unknown")))
    }
}

fn strip_directives_and_comments(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_init = false;
    for line in source.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("%%") && !t.starts_with("%%{") {
            continue;
        }
        if t.starts_with("%%{") {
            if !t.contains("}%%") {
                in_init = true;
            }
            continue;
        }
        if in_init {
            if t.contains("}%%") {
                in_init = false;
            }
            continue;
        }
        out.push(t.to_owned());
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowDir {
    Td,
    Lr,
    Bt,
    Rl,
}

fn parse_flow_direction(header: &str) -> FlowDir {
    let parts: Vec<&str> = header.split_whitespace().collect();
    match parts.get(1).map(|s| s.trim_end_matches(';').to_ascii_uppercase()).as_deref() {
        Some("LR") => FlowDir::Lr,
        Some("RL") => FlowDir::Rl,
        Some("BT") => FlowDir::Bt,
        _ => FlowDir::Td,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeShape {
    Rect,
    Round,
    Pill,
    Diamond,
    Circle,
}

#[derive(Debug, Clone)]
struct GraphNode {
    id: String,
    label: String,
    shape: NodeShape,
    subgraph: Option<String>,
    subtitle: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeStyle {
    Solid,
    Dashed,
    Thick,
}

#[derive(Debug, Clone)]
struct GraphEdge {
    from: String,
    to: String,
    label: Option<String>,
    directed: bool,
    style: EdgeStyle,
}

fn render_flowchart(lines: &[String], dir: FlowDir, p: MermaidPalette) -> Result<RenderedMermaidSvg, String> {
    let mut nodes: HashMap<String, GraphNode> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut subgraph_stack: Vec<String> = Vec::new();

    for raw_line in lines {
        for stmt in split_mermaid_statements(raw_line) {
            let trimmed = stmt.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("classDef ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("style ")
                || trimmed.starts_with("linkStyle ")
                || trimmed.starts_with("click ")
            {
                continue;
            }
            if let Some(rest) = strip_keyword_ci(trimmed, "subgraph") {
                let title = rest.trim().trim_matches('"').to_owned();
                subgraph_stack.push(if title.is_empty() { "Group".to_owned() } else { title });
                continue;
            }
            if trimmed.eq_ignore_ascii_case("end") {
                subgraph_stack.pop();
                continue;
            }
            if trimmed.starts_with("direction ") {
                continue;
            }

            let sg = subgraph_stack.last().cloned();
            if let Some(chain) = parse_flow_edge_chain(trimmed) {
                for (lhs, edge_spec, rhs) in chain {
                    let from = upsert_node(&mut nodes, &mut order, &lhs, sg.as_deref());
                    let to = upsert_node(&mut nodes, &mut order, &rhs, sg.as_deref());
                    edges.push(GraphEdge {
                        from,
                        to,
                        label: edge_spec.label,
                        directed: edge_spec.directed,
                        style: edge_spec.style,
                    });
                }
            } else {
                // Support `A & B --> C` or single node declarations.
                let _ = upsert_node(&mut nodes, &mut order, trimmed, sg.as_deref());
            }
        }
    }

    if nodes.is_empty() {
        return Err("No nodes found in flowchart.".into());
    }

    layout_and_render_graph(&nodes, &order, &edges, dir, p)
}

fn render_state_diagram(lines: &[String], p: MermaidPalette) -> Result<RenderedMermaidSvg, String> {
    let mut nodes: HashMap<String, GraphNode> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut dir = FlowDir::Td;
    let mut start_count = 0usize;

    for raw in lines {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with("note ") || trimmed.starts_with("end note") {
            continue;
        }
        if let Some(rest) = strip_keyword_ci(trimmed, "direction") {
            match rest.trim().to_ascii_uppercase().as_str() {
                "LR" => dir = FlowDir::Lr,
                "RL" => dir = FlowDir::Rl,
                "BT" => dir = FlowDir::Bt,
                _ => dir = FlowDir::Td,
            }
            continue;
        }
        if trimmed == "{" || trimmed == "}" || trimmed.starts_with("state ") && !trimmed.contains("-->") {
            if let Some(rest) = strip_keyword_ci(trimmed, "state")
                && let Some((label, id)) = rest.split_once(" as ")
            {
                let id = id.trim().trim_end_matches('{').trim();
                let label = label.trim().trim_matches('"');
                insert_custom_node(&mut nodes, &mut order, id, label, NodeShape::Round, None);
            }
            continue;
        }
        if let Some((lhs_raw, rhs_part)) = trimmed.split_once("-->") {
            let (rhs_raw, label) = match rhs_part.split_once(':') {
                Some((r, l)) => (r.trim(), Some(l.trim().to_owned()).filter(|s| !s.is_empty())),
                None => (rhs_part.trim(), None),
            };
            let from = if lhs_raw.trim() == "[*]" {
                start_count += 1;
                let id = format!("__start_{start_count}");
                insert_custom_node(&mut nodes, &mut order, &id, "", NodeShape::Circle, None);
                id
            } else {
                let id = lhs_raw.split_whitespace().next().unwrap_or(lhs_raw.trim());
                ensure_node_exists(&mut nodes, &mut order, id, NodeShape::Round);
                id.to_owned()
            };
            let to = if rhs_raw == "[*]" {
                start_count += 1;
                let id = format!("__end_{start_count}");
                insert_custom_node(&mut nodes, &mut order, &id, "●", NodeShape::Circle, None);
                id
            } else {
                let id = rhs_raw.split_whitespace().next().unwrap_or(rhs_raw);
                ensure_node_exists(&mut nodes, &mut order, id, NodeShape::Round);
                id.to_owned()
            };
            edges.push(GraphEdge {
                from,
                to,
                label,
                directed: true,
                style: EdgeStyle::Solid,
            });
        }
    }

    if nodes.is_empty() {
        return Err("No states found in stateDiagram.".into());
    }
    layout_and_render_graph(&nodes, &order, &edges, dir, p)
}

fn render_box_diagram(lines: &[String], p: MermaidPalette) -> Result<RenderedMermaidSvg, String> {
    let mut nodes: HashMap<String, GraphNode> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut current_block: Option<String> = None;

    for raw in lines {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(ref block_id) = current_block {
            if trimmed == "}" {
                current_block = None;
            } else if let Some(node) = nodes.get_mut(block_id) {
                node.subtitle.push(trimmed.to_owned());
            }
            continue;
        }
        if let Some(rest) = strip_keyword_ci(trimmed, "class").or_else(|| trimmed.ends_with('{').then_some(trimmed)) {
            let clean = rest.trim_end_matches('{').trim();
            let id = clean.split_whitespace().next().unwrap_or(clean);
            if !id.is_empty() {
                insert_custom_node(&mut nodes, &mut order, id, id, NodeShape::Rect, None);
                if trimmed.ends_with('{') {
                    current_block = Some(id.to_owned());
                }
                continue;
            }
        }
        // Class / ER relationship line: `A <|-- B : label`, `USER ||--o{ ORDER : places`
        if let Some((lhs, rhs, label, directed)) = parse_relation_line(trimmed) {
            ensure_node_exists(&mut nodes, &mut order, &lhs, NodeShape::Rect);
            ensure_node_exists(&mut nodes, &mut order, &rhs, NodeShape::Rect);
            edges.push(GraphEdge {
                from: lhs,
                to: rhs,
                label,
                directed,
                style: EdgeStyle::Solid,
            });
        }
    }

    if nodes.is_empty() {
        return Err("No entities found in diagram.".into());
    }
    layout_and_render_graph(&nodes, &order, &edges, FlowDir::Td, p)
}

fn parse_relation_line(line: &str) -> Option<(String, String, Option<String>, bool)> {
    let (body, label) = match line.split_once(':') {
        Some((b, l)) => (b.trim(), Some(l.trim().to_owned()).filter(|s| !s.is_empty())),
        None => (line.trim(), None),
    };
    let ops = [
        "<|--", "--|>", "*--", "--*", "o--", "--o", "<..", "..>", "<--", "-->", "--", "..",
        "||--o{", "}o--||", "||--|{", "}|--||", "||--||", "}o--o{", "|o--o|",
    ];
    for op in ops {
        if let Some((l, r)) = body.split_once(op) {
            let lhs = l.trim().trim_matches('"').split_whitespace().last()?.to_owned();
            let rhs = r.trim().trim_matches('"').split_whitespace().next()?.to_owned();
            if !lhs.is_empty() && !rhs.is_empty() {
                return Some((lhs, rhs, label, true));
            }
        }
    }
    None
}

fn render_sequence(lines: &[String], p: MermaidPalette) -> Result<RenderedMermaidSvg, String> {
    let mut participants: Vec<(String, String)> = Vec::new();
    let mut index_of: HashMap<String, usize> = HashMap::new();
    struct Msg {
        from: usize,
        to: usize,
        label: String,
        dashed: bool,
    }
    let mut messages: Vec<Msg> = Vec::new();

    let mut ensure_part = |id: &str, alias: Option<&str>| -> usize {
        if let Some(&idx) = index_of.get(id) {
            if let Some(a) = alias {
                participants[idx].1 = a.to_owned();
            }
            return idx;
        }
        let idx = participants.len();
        participants.push((id.to_owned(), alias.unwrap_or(id).to_owned()));
        index_of.insert(id.to_owned(), idx);
        idx
    };

    for raw in lines {
        let trimmed = raw.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("activate ")
            || trimmed.starts_with("deactivate ")
            || trimmed.starts_with("note ")
            || trimmed.starts_with("loop ")
            || trimmed.starts_with("alt ")
            || trimmed.starts_with("else ")
            || trimmed.starts_with("opt ")
            || trimmed == "end"
            || trimmed.starts_with("autonumber")
        {
            continue;
        }
        if let Some(rest) = strip_keyword_ci(trimmed, "participant").or_else(|| strip_keyword_ci(trimmed, "actor")) {
            if let Some((id, alias)) = rest.split_once(" as ") {
                ensure_part(id.trim(), Some(alias.trim()));
            } else {
                ensure_part(rest.trim(), None);
            }
            continue;
        }
        if let Some((body, text)) = trimmed.split_once(':') {
            let arrows = ["-->>", "->>", "-->", "->", "--)", "-)"];
            if let Some((op, l, r)) = arrows.iter().find_map(|op| body.split_once(op).map(|(l, r)| (*op, l, r))) {
                let from_id = l.trim();
                let to_id = r.trim();
                if !from_id.is_empty() && !to_id.is_empty() {
                    let from = ensure_part(from_id, None);
                    let to = ensure_part(to_id, None);
                    messages.push(Msg {
                        from,
                        to,
                        label: text.trim().to_owned(),
                        dashed: op.starts_with("--"),
                    });
                }
            }
        }
    }

    if participants.is_empty() {
        return Err("No participants found in sequenceDiagram.".into());
    }

    let pad = 24.0_f64;
    let box_w = participants
        .iter()
        .map(|(_, l)| (l.chars().count() as f64 * 8.0 + 28.0).max(110.0))
        .fold(110.0, f64::max);
    let col_gap = 42.0_f64;
    let step_h = 46.0_f64;
    let box_h = 36.0_f64;
    let top_y = pad;
    let lifeline_start = top_y + box_h;
    let body_h = (messages.len().max(1) as f64) * step_h + 24.0;
    let bottom_y = lifeline_start + body_h;
    let total_w = pad * 2.0 + participants.len() as f64 * box_w + (participants.len().saturating_sub(1)) as f64 * col_gap;
    let total_h = bottom_y + box_h + pad;

    let col_x = |idx: usize| -> f64 { pad + idx as f64 * (box_w + col_gap) + box_w / 2.0 };

    let mut svg = svg_open(total_w, total_h, p.background);

    // Lifelines
    for idx in 0..participants.len() {
        let x = col_x(idx);
        svg.push_str(&format!(
            r#"<line x1="{x:.1}" y1="{lifeline_start:.1}" x2="{x:.1}" y2="{bottom_y:.1}" stroke="{}" stroke-width="1.2" stroke-dasharray="5,4"/>"#,
            p.border
        ));
    }

    // Participant top + bottom boxes
    for (idx, (_, label)) in participants.iter().enumerate() {
        let cx = col_x(idx);
        let rx = cx - box_w / 2.0;
        for y in [top_y, bottom_y] {
            svg.push_str(&format!(
                r#"<rect x="{rx:.1}" y="{y:.1}" width="{box_w:.1}" height="{box_h:.1}" rx="6" fill="{}" stroke="{}" stroke-width="1.3"/>"#,
                p.surface, p.border
            ));
            let ty = y + box_h / 2.0 + 4.5;
            svg.push_str(&format!(
                r#"<text x="{cx:.1}" y="{ty:.1}" text-anchor="middle" font-family="sans-serif" font-size="12.5" font-weight="600" fill="{}">{}</text>"#,
                p.text,
                escape_xml(label)
            ));
        }
    }

    // Messages
    for (i, msg) in messages.iter().enumerate() {
        let y = lifeline_start + 32.0 + i as f64 * step_h;
        let x1 = col_x(msg.from);
        let x2 = col_x(msg.to);
        let dash = if msg.dashed { r#" stroke-dasharray="5,4""# } else { "" };
        if msg.from == msg.to {
            let loop_w = 38.0;
            svg.push_str(&format!(
                r#"<path d="M {x1:.1} {y:.1} L {:.1} {y:.1} L {:.1} {:.1} L {x1:.1} {:.1}" fill="none" stroke="{}" stroke-width="1.5"{dash}/>"#,
                x1 + loop_w,
                x1 + loop_w,
                y + 20.0,
                y + 20.0,
                p.edge
            ));
            svg.push_str(&arrow_head(x1 + 10.0, y + 20.0, x1, y + 20.0, p.edge));
            svg.push_str(&format!(
                r#"<text x="{:.1}" y="{:.1}" text-anchor="start" font-family="sans-serif" font-size="11.5" fill="{}">{}</text>"#,
                x1 + loop_w + 6.0,
                y + 13.0,
                p.text,
                escape_xml(&msg.label)
            ));
        } else {
            let tx = if x2 > x1 { x2 - 2.0 } else { x2 + 2.0 };
            svg.push_str(&format!(
                r#"<line x1="{x1:.1}" y1="{y:.1}" x2="{tx:.1}" y2="{y:.1}" stroke="{}" stroke-width="1.5"{dash}/>"#,
                p.edge
            ));
            svg.push_str(&arrow_head(x1, y, x2, y, p.edge));
            let mid = (x1 + x2) / 2.0;
            svg.push_str(&format!(
                r#"<text x="{mid:.1}" y="{:.1}" text-anchor="middle" font-family="sans-serif" font-size="11.5" fill="{}">{}</text>"#,
                y - 7.0,
                p.text,
                escape_xml(&msg.label)
            ));
        }
    }

    svg.push_str("</svg>");
    Ok(RenderedMermaidSvg {
        svg,
        width: total_w as f32,
        height: total_h as f32,
    })
}

fn render_pie(header: &str, lines: &[String], p: MermaidPalette) -> Result<RenderedMermaidSvg, String> {
    let title = header
        .split_once("title")
        .map(|(_, t)| t.trim().trim_matches('"').to_owned())
        .filter(|t| !t.is_empty());

    let mut slices: Vec<(String, f64)> = Vec::new();
    let mut resolved_title = title;
    for raw in lines {
        let trimmed = raw.trim();
        if let Some(rest) = strip_keyword_ci(trimmed, "title") {
            let t = rest.trim().trim_matches('"');
            if !t.is_empty() {
                resolved_title = Some(t.to_owned());
            }
            continue;
        }
        if let Some((lbl, val)) = trimmed.split_once(':') {
            let label = lbl.trim().trim_matches('"').to_owned();
            if let Ok(num) = val.trim().parse::<f64>()
                && num > 0.0
            {
                slices.push((label, num));
            }
        }
    }

    let total: f64 = slices.iter().map(|(_, v)| *v).sum();
    if slices.is_empty() || total <= 0.0 {
        return Err("No slices found in pie diagram.".into());
    }

    let width = 480.0_f64;
    let title_h = if resolved_title.is_some() { 34.0 } else { 12.0 };
    let r = 84.0_f64;
    let cx = 128.0_f64;
    let cy = title_h + 18.0 + r;
    let legend_h = slices.len() as f64 * 24.0 + 24.0;
    let height = (cy + r + 24.0).max(title_h + legend_h + 24.0);

    let mut svg = svg_open(width, height, p.background);
    if let Some(t) = &resolved_title {
        svg.push_str(&format!(
            r#"<text x="{:.1}" y="24" text-anchor="middle" font-family="sans-serif" font-size="14" font-weight="600" fill="{}">{}</text>"#,
            width / 2.0,
            p.text,
            escape_xml(t)
        ));
    }

    let mut angle = -PI / 2.0;
    for (idx, (label, value)) in slices.iter().enumerate() {
        let color = p.series[idx % p.series.len()];
        let frac = *value / total;
        let sweep = frac * 2.0 * PI;
        if slices.len() == 1 {
            svg.push_str(&format!(
                r#"<circle cx="{cx:.1}" cy="{cy:.1}" r="{r:.1}" fill="{color}" stroke="{}" stroke-width="1.5"/>"#,
                p.background
            ));
        } else {
            let x1 = cx + r * angle.cos();
            let y1 = cy + r * angle.sin();
            let next_angle = angle + sweep;
            let x2 = cx + r * next_angle.cos();
            let y2 = cy + r * next_angle.sin();
            let large_arc = if sweep > PI { 1 } else { 0 };
            svg.push_str(&format!(
                r#"<path d="M {cx:.1} {cy:.1} L {x1:.1} {y1:.1} A {r:.1} {r:.1} 0 {large_arc} 1 {x2:.1} {y2:.1} Z" fill="{color}" stroke="{}" stroke-width="1.5"/>"#,
                p.background
            ));
            angle = next_angle;
        }

        let lx = 246.0;
        let ly = title_h + 28.0 + idx as f64 * 24.0;
        svg.push_str(&format!(
            r#"<rect x="{lx:.1}" y="{:.1}" width="12" height="12" rx="3" fill="{color}"/>"#,
            ly - 10.0
        ));
        svg.push_str(&format!(
            r#"<text x="{:.1}" y="{ly:.1}" font-family="sans-serif" font-size="12" fill="{}">{} ({:.1}%)</text>"#,
            lx + 18.0,
            p.text,
            escape_xml(label),
            frac * 100.0
        ));
    }

    svg.push_str("</svg>");
    Ok(RenderedMermaidSvg {
        svg,
        width: width as f32,
        height: height as f32,
    })
}

#[derive(Debug, Clone)]
struct EdgeSpec {
    label: Option<String>,
    directed: bool,
    style: EdgeStyle,
}

fn parse_flow_edge_chain(stmt: &str) -> Option<Vec<(String, EdgeSpec, String)>> {
    // Tokenize connectors (`-->`, `-.->`, `==>`, `---`, `-.->|label|`, `-- label -->`, etc.).
    let mut segments: Vec<String> = Vec::new();
    let mut specs: Vec<EdgeSpec> = Vec::new();
    let mut rest = stmt;

    while let Some((before, spec, after)) = find_next_flow_connector(rest) {
        let lhs = before.trim();
        if lhs.is_empty() {
            return None;
        }
        segments.push(lhs.to_owned());
        specs.push(spec);
        rest = after;
    }

    if specs.is_empty() {
        return None;
    }
    let tail = rest.trim();
    if tail.is_empty() {
        return None;
    }
    segments.push(tail.to_owned());

    let mut pairs = Vec::new();
    for (idx, spec) in specs.into_iter().enumerate() {
        for lhs_item in split_flow_group(&segments[idx]) {
            for rhs_item in split_flow_group(&segments[idx + 1]) {
                pairs.push((lhs_item.to_owned(), spec.clone(), rhs_item.to_owned()));
            }
        }
    }
    (!pairs.is_empty()).then_some(pairs)
}

fn split_flow_group(segment: &str) -> Vec<&str> {
    let bytes = segment.as_bytes();
    let (mut start, mut bracket, mut paren, mut brace, mut in_quote) = (0usize, 0usize, 0usize, 0usize, false);
    let mut parts = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'"' {
            in_quote = !in_quote;
        } else if !in_quote {
            match b {
                b'[' => bracket += 1,
                b']' => bracket = bracket.saturating_sub(1),
                b'(' => paren += 1,
                b')' => paren = paren.saturating_sub(1),
                b'{' => brace += 1,
                b'}' => brace = brace.saturating_sub(1),
                b'&' if bracket == 0 && paren == 0 && brace == 0 => {
                    let piece = segment[start..i].trim();
                    if !piece.is_empty() {
                        parts.push(piece);
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }
    }
    let tail = segment[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

fn find_next_flow_connector(input: &str) -> Option<(&str, EdgeSpec, &str)> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut bracket_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut in_quote = false;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            in_quote = !in_quote;
            i += 1;
            continue;
        }
        if in_quote {
            i += 1;
            continue;
        }
        match b {
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            _ => {}
        }
        if bracket_depth == 0
            && paren_depth == 0
            && brace_depth == 0
            && let Some((op_len, spec, after_start)) = match_connector_at(&input[i..])
        {
            let _ = op_len;
            return Some((&input[..i], spec, &input[i + after_start..]));
        }
        i += 1;
    }
    None
}

fn match_connector_at(s: &str) -> Option<(usize, EdgeSpec, usize)> {
    // Check inline text label forms: `-- text -->`, `-. text .->`, `== text ==>`.
    for (open, close, style, directed) in [
        ("-- ", " -->", EdgeStyle::Solid, true),
        ("-- ", " ---", EdgeStyle::Solid, false),
        ("-. ", " .->", EdgeStyle::Dashed, true),
        ("== ", " ==>", EdgeStyle::Thick, true),
    ] {
        if let Some(after_open) = s.strip_prefix(open)
            && let Some(end_idx) = after_open.find(close)
            && !after_open[..end_idx].contains(['[', ']', '\n'])
        {
            let label = after_open[..end_idx].trim().trim_matches('"').to_owned();
            let consumed = open.len() + end_idx + close.len();
            return Some((
                consumed,
                EdgeSpec {
                    label: (!label.is_empty()).then_some(label),
                    directed,
                    style,
                },
                consumed,
            ));
        }
    }

    let ops = [
        ("-.->", EdgeStyle::Dashed, true),
        ("-.-", EdgeStyle::Dashed, false),
        ("==>", EdgeStyle::Thick, true),
        ("===", EdgeStyle::Thick, false),
        ("-->", EdgeStyle::Solid, true),
        ("---", EdgeStyle::Solid, false),
        ("->", EdgeStyle::Solid, true),
    ];
    for (op, style, directed) in ops {
        if let Some(after_op) = s.strip_prefix(op) {
            let trimmed_ws = after_op.trim_start();
            let ws_len = after_op.len() - trimmed_ws.len();
            if let Some(after_pipe) = trimmed_ws.strip_prefix('|')
                && let Some(close_pipe) = after_pipe.find('|')
            {
                let label = after_pipe[..close_pipe].trim().trim_matches('"').to_owned();
                let consumed = op.len() + ws_len + 1 + close_pipe + 1;
                return Some((
                    op.len(),
                    EdgeSpec {
                        label: (!label.is_empty()).then_some(label),
                        directed,
                        style,
                    },
                    consumed,
                ));
            }
            return Some((
                op.len(),
                EdgeSpec { label: None, directed, style },
                op.len(),
            ));
        }
    }
    None
}

fn upsert_node(
    nodes: &mut HashMap<String, GraphNode>,
    order: &mut Vec<String>,
    token: &str,
    subgraph: Option<&str>,
) -> String {
    let (id, label, shape) = parse_node_token(token);
    if let Some(existing) = nodes.get_mut(&id) {
        if label != id {
            existing.label = label;
            existing.shape = shape;
        }
        if existing.subgraph.is_none() {
            existing.subgraph = subgraph.map(str::to_owned);
        }
    } else {
        order.push(id.clone());
        nodes.insert(
            id.clone(),
            GraphNode {
                id: id.clone(),
                label,
                shape,
                subgraph: subgraph.map(str::to_owned),
                subtitle: Vec::new(),
            },
        );
    }
    id
}

fn ensure_node_exists(nodes: &mut HashMap<String, GraphNode>, order: &mut Vec<String>, id: &str, shape: NodeShape) {
    if !nodes.contains_key(id) {
        order.push(id.to_owned());
        nodes.insert(
            id.to_owned(),
            GraphNode {
                id: id.to_owned(),
                label: id.to_owned(),
                shape,
                subgraph: None,
                subtitle: Vec::new(),
            },
        );
    }
}

fn insert_custom_node(
    nodes: &mut HashMap<String, GraphNode>,
    order: &mut Vec<String>,
    id: &str,
    label: &str,
    shape: NodeShape,
    subgraph: Option<&str>,
) {
    if !nodes.contains_key(id) {
        order.push(id.to_owned());
    }
    nodes.insert(
        id.to_owned(),
        GraphNode {
            id: id.to_owned(),
            label: label.to_owned(),
            shape,
            subgraph: subgraph.map(str::to_owned),
            subtitle: Vec::new(),
        },
    );
}

fn parse_node_token(raw: &str) -> (String, String, NodeShape) {
    let stripped_class = raw.split(":::").next().unwrap_or(raw).trim();
    let shapes = [
        ("((", "))", NodeShape::Circle),
        ("([", "])", NodeShape::Pill),
        ("[[", "]]", NodeShape::Rect),
        ("[(", ")]", NodeShape::Round),
        ("[", "]", NodeShape::Rect),
        ("(", ")", NodeShape::Round),
        ("{", "}", NodeShape::Diamond),
    ];
    for (open, close, shape) in shapes {
        if let Some(idx) = stripped_class.find(open) {
            let id = stripped_class[..idx].trim();
            let rest = &stripped_class[idx + open.len()..];
            if let Some(end_rel) = rest.rfind(close) {
                let raw_label = rest[..end_rel].trim().trim_matches('"').trim_matches('`');
                let final_id = if id.is_empty() { raw_label } else { id };
                let final_label = if raw_label.is_empty() { final_id } else { raw_label };
                return (final_id.to_owned(), final_label.to_owned(), shape);
            }
        }
    }
    let clean = stripped_class.split_whitespace().next().unwrap_or(stripped_class);
    (clean.to_owned(), clean.to_owned(), NodeShape::Rect)
}

fn layout_and_render_graph(
    nodes: &HashMap<String, GraphNode>,
    order: &[String],
    edges: &[GraphEdge],
    dir: FlowDir,
    p: MermaidPalette,
) -> Result<RenderedMermaidSvg, String> {
    // Compute acyclic ranks via Kahn-style relaxation with cycle protection.
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut indegree: HashMap<&str, usize> = order.iter().map(|id| (id.as_str(), 0)).collect();
    for edge in edges {
        outgoing.entry(edge.from.as_str()).or_default().push(edge.to.as_str());
        if edge.from != edge.to
            && let Some(cnt) = indegree.get_mut(edge.to.as_str())
        {
            *cnt += 1;
        }
    }

    let mut rank: HashMap<&str, usize> = order.iter().map(|id| (id.as_str(), 0)).collect();
    let mut queue: Vec<&str> = order.iter().filter(|id| indegree[id.as_str()] == 0).map(String::as_str).collect();
    let mut visited = HashSet::new();
    let mut idx = 0;
    while idx < queue.len() {
        let u = queue[idx];
        idx += 1;
        if !visited.insert(u) {
            continue;
        }
        let u_rank = rank[u];
        if let Some(nexts) = outgoing.get(u) {
            for &v in nexts {
                if u == v {
                    continue;
                }
                let next_rank = u_rank + 1;
                if next_rank > rank[v] && next_rank < order.len() + 2 {
                    rank.insert(v, next_rank);
                }
                if let Some(d) = indegree.get_mut(v) {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        queue.push(v);
                    }
                }
            }
        }
    }
    // Relax remaining nodes in case of cycles.
    for _ in 0..order.len().min(8) {
        for edge in edges {
            if edge.from != edge.to && !visited.contains(edge.to.as_str()) {
                let candidate = rank[edge.from.as_str()] + 1;
                if candidate > rank[edge.to.as_str()] && candidate <= order.len() {
                    rank.insert(edge.to.as_str(), candidate);
                }
            }
        }
    }

    let max_rank = rank.values().copied().max().unwrap_or(0);
    let mut layers: Vec<Vec<&GraphNode>> = vec![Vec::new(); max_rank + 1];
    for id in order {
        let r = rank[id.as_str()];
        let actual_r = match dir {
            FlowDir::Bt | FlowDir::Rl => max_rank.saturating_sub(r),
            FlowDir::Td | FlowDir::Lr => r,
        };
        if let Some(node) = nodes.get(id) {
            layers[actual_r].push(node);
        }
    }

    let horizontal = matches!(dir, FlowDir::Lr | FlowDir::Rl);
    let mut box_dims: HashMap<&str, (f64, f64)> = HashMap::new();
    for id in order {
        let node = &nodes[id];
        let max_chars = std::iter::once(node.label.chars().count())
            .chain(node.subtitle.iter().map(|s| s.chars().count()))
            .max()
            .unwrap_or(4);
        let (w, h) = match node.shape {
            NodeShape::Circle if node.label.chars().count() <= 1 => (26.0, 26.0),
            NodeShape::Diamond => ((max_chars as f64 * 7.8 + 44.0).max(90.0), 54.0),
            _ => {
                let w = (max_chars as f64 * 7.8 + 32.0).max(88.0);
                let h = if node.subtitle.is_empty() { 40.0 } else { 36.0 + node.subtitle.len() as f64 * 17.0 };
                (w, h)
            }
        };
        box_dims.insert(id.as_str(), (w, h));
    }

    let pad = 28.0_f64;
    let rank_gap = 62.0_f64;
    let item_gap = 28.0_f64;
    let mut centers: HashMap<&str, (f64, f64)> = HashMap::new();

    let (total_w, total_h) = if horizontal {
        let layer_widths: Vec<f64> = layers
            .iter()
            .map(|l| l.iter().map(|n| box_dims[n.id.as_str()].0).fold(0.0, f64::max))
            .collect();
        let layer_heights: Vec<f64> = layers
            .iter()
            .map(|l| {
                let sum: f64 = l.iter().map(|n| box_dims[n.id.as_str()].1).sum();
                sum + item_gap * (l.len().saturating_sub(1)) as f64
            })
            .collect();
        let max_col_h = layer_heights.iter().copied().fold(80.0, f64::max);
        let mut x = pad;
        for (li, layer) in layers.iter().enumerate() {
            let lw = layer_widths[li];
            let mut y = pad + (max_col_h - layer_heights[li]) / 2.0;
            for node in layer {
                let (_, nh) = box_dims[node.id.as_str()];
                centers.insert(node.id.as_str(), (x + lw / 2.0, y + nh / 2.0));
                y += nh + item_gap;
            }
            x += lw + rank_gap;
        }
        (x - rank_gap + pad, max_col_h + pad * 2.0)
    } else {
        let layer_heights: Vec<f64> = layers
            .iter()
            .map(|l| l.iter().map(|n| box_dims[n.id.as_str()].1).fold(0.0, f64::max))
            .collect();
        let layer_widths: Vec<f64> = layers
            .iter()
            .map(|l| {
                let sum: f64 = l.iter().map(|n| box_dims[n.id.as_str()].0).sum();
                sum + item_gap * (l.len().saturating_sub(1)) as f64
            })
            .collect();
        let max_row_w = layer_widths.iter().copied().fold(140.0, f64::max);
        let mut y = pad;
        for (li, layer) in layers.iter().enumerate() {
            let lh = layer_heights[li];
            let mut x = pad + (max_row_w - layer_widths[li]) / 2.0;
            for node in layer {
                let (nw, _) = box_dims[node.id.as_str()];
                centers.insert(node.id.as_str(), (x + nw / 2.0, y + lh / 2.0));
                x += nw + item_gap;
            }
            y += lh + rank_gap;
        }
        (max_row_w + pad * 2.0, y - rank_gap + pad)
    };

    let mut svg = svg_open(total_w, total_h, p.background);

    // Subgraph background frames
    let mut sg_bounds: HashMap<&str, (f64, f64, f64, f64)> = HashMap::new();
    for id in order {
        let node = &nodes[id];
        if let Some(ref sg) = node.subgraph {
            let (cx, cy) = centers[id.as_str()];
            let (w, h) = box_dims[id.as_str()];
            let (min_x, min_y, max_x, max_y) = (cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0);
            sg_bounds
                .entry(sg.as_str())
                .and_modify(|b| {
                    b.0 = b.0.min(min_x);
                    b.1 = b.1.min(min_y);
                    b.2 = b.2.max(max_x);
                    b.3 = b.3.max(max_y);
                })
                .or_insert((min_x, min_y, max_x, max_y));
        }
    }
    for (title, (x1, y1, x2, y2)) in sg_bounds {
        let sx = x1 - 14.0;
        let sy = y1 - 24.0;
        let sw = (x2 - x1) + 28.0;
        let sh = (y2 - y1) + 38.0;
        svg.push_str(&format!(
            r#"<rect x="{sx:.1}" y="{sy:.1}" width="{sw:.1}" height="{sh:.1}" rx="8" fill="{}" fill-opacity="0.45" stroke="{}" stroke-dasharray="4,3" stroke-width="1"/>"#,
            p.alt_surface, p.border
        ));
        svg.push_str(&format!(
            r#"<text x="{:.1}" y="{:.1}" font-family="sans-serif" font-size="11" font-weight="600" fill="{}">{}</text>"#,
            sx + 10.0,
            sy + 15.0,
            p.muted,
            escape_xml(title)
        ));
    }

    // Edges behind nodes
    for edge in edges {
        let (cx1, cy1) = centers[edge.from.as_str()];
        let (w1, h1) = box_dims[edge.from.as_str()];
        let (cx2, cy2) = centers[edge.to.as_str()];
        let (w2, h2) = box_dims[edge.to.as_str()];
        let (sx, sy) = rect_boundary_point(cx1, cy1, w1, h1, cx2, cy2);
        let (ex, ey) = rect_boundary_point(cx2, cy2, w2, h2, cx1, cy1);
        let sw = match edge.style {
            EdgeStyle::Thick => "2.4",
            _ => "1.5",
        };
        let dash = if edge.style == EdgeStyle::Dashed {
            r#" stroke-dasharray="5,4""#
        } else {
            ""
        };
        let (cpx1, cpy1, cpx2, cpy2) = if horizontal {
            let mx = (sx + ex) / 2.0;
            (mx, sy, mx, ey)
        } else {
            let my = (sy + ey) / 2.0;
            (sx, my, ex, my)
        };
        svg.push_str(&format!(
            r#"<path d="M {sx:.1} {sy:.1} C {cpx1:.1} {cpy1:.1}, {cpx2:.1} {cpy2:.1}, {ex:.1} {ey:.1}" fill="none" stroke="{}" stroke-width="{sw}"{dash}/>"#,
            p.edge
        ));
        if edge.directed {
            svg.push_str(&arrow_head(cpx2, cpy2, ex, ey, p.edge));
        }
        if let Some(ref lbl) = edge.label {
            let lx = (sx + ex) / 2.0;
            let ly = (sy + ey) / 2.0;
            let lw = (lbl.chars().count() as f64 * 6.8 + 12.0).max(28.0);
            svg.push_str(&format!(
                r#"<rect x="{:.1}" y="{:.1}" width="{lw:.1}" height="18" rx="4" fill="{}" stroke="{}" stroke-width="0.8"/>"#,
                lx - lw / 2.0,
                ly - 9.0,
                p.background,
                p.border
            ));
            svg.push_str(&format!(
                r#"<text x="{lx:.1}" y="{:.1}" text-anchor="middle" font-family="sans-serif" font-size="11" fill="{}">{}</text>"#,
                ly + 3.5,
                p.muted,
                escape_xml(lbl)
            ));
        }
    }

    // Nodes
    for id in order {
        let node = &nodes[id];
        let (cx, cy) = centers[id.as_str()];
        let (w, h) = box_dims[id.as_str()];
        let x = cx - w / 2.0;
        let y = cy - h / 2.0;
        match node.shape {
            NodeShape::Rect | NodeShape::Round | NodeShape::Pill => {
                let rx = match node.shape {
                    NodeShape::Pill => h / 2.0,
                    NodeShape::Round => 10.0,
                    _ => 5.0,
                };
                svg.push_str(&format!(
                    r#"<rect x="{x:.1}" y="{y:.1}" width="{w:.1}" height="{h:.1}" rx="{rx:.1}" fill="{}" stroke="{}" stroke-width="1.4"/>"#,
                    p.surface, p.border
                ));
            }
            NodeShape::Diamond => {
                let top = (cx, y);
                let right = (x + w, cy);
                let bottom = (cx, y + h);
                let left = (x, cy);
                svg.push_str(&format!(
                    r#"<polygon points="{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}" fill="{}" stroke="{}" stroke-width="1.4"/>"#,
                    top.0, top.1, right.0, right.1, bottom.0, bottom.1, left.0, left.1, p.surface, p.accent
                ));
            }
            NodeShape::Circle => {
                let r = (w.min(h)) / 2.0;
                let fill = if node.label.is_empty() { p.accent } else { p.surface };
                svg.push_str(&format!(
                    r#"<circle cx="{cx:.1}" cy="{cy:.1}" r="{r:.1}" fill="{fill}" stroke="{}" stroke-width="1.5"/>"#,
                    p.accent
                ));
            }
        }
        if !node.label.is_empty() {
            let label_y = if node.subtitle.is_empty() {
                cy + 4.5
            } else {
                y + 18.0
            };
            svg.push_str(&format!(
                r#"<text x="{cx:.1}" y="{label_y:.1}" text-anchor="middle" font-family="sans-serif" font-size="12.5" font-weight="600" fill="{}">{}</text>"#,
                p.text,
                escape_xml(&node.label)
            ));
            if !node.subtitle.is_empty() {
                svg.push_str(&format!(
                    r#"<line x1="{x:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="{}" stroke-width="1"/>"#,
                    y + 26.0,
                    x + w,
                    y + 26.0,
                    p.border
                ));
                for (si, sub) in node.subtitle.iter().enumerate() {
                    svg.push_str(&format!(
                        r#"<text x="{:.1}" y="{:.1}" font-family="sans-serif" font-size="11" fill="{}">{}</text>"#,
                        x + 10.0,
                        y + 42.0 + si as f64 * 16.0,
                        p.muted,
                        escape_xml(sub)
                    ));
                }
            }
        }
    }

    svg.push_str("</svg>");
    Ok(RenderedMermaidSvg {
        svg,
        width: total_w as f32,
        height: total_h as f32,
    })
}

fn rect_boundary_point(cx: f64, cy: f64, w: f64, h: f64, tx: f64, ty: f64) -> (f64, f64) {
    let dx = tx - cx;
    let dy = ty - cy;
    if dx.abs() < 0.001 && dy.abs() < 0.001 {
        return (cx, cy);
    }
    let hw = w / 2.0;
    let hh = h / 2.0;
    let sx = if dx.abs() > 0.001 { hw / dx.abs() } else { f64::INFINITY };
    let sy = if dy.abs() > 0.001 { hh / dy.abs() } else { f64::INFINITY };
    let t = sx.min(sy);
    (cx + dx * t, cy + dy * t)
}

fn arrow_head(from_x: f64, from_y: f64, to_x: f64, to_y: f64, color: &str) -> String {
    let dx = to_x - from_x;
    let dy = to_y - from_y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.001 {
        return String::new();
    }
    let ux = dx / len;
    let uy = dy / len;
    let size = 8.0;
    let wing = 4.2;
    let bx = to_x - ux * size;
    let by = to_y - uy * size;
    let px = -uy * wing;
    let py = ux * wing;
    format!(
        r#"<polygon points="{to_x:.1},{to_y:.1} {:.1},{:.1} {:.1},{:.1}" fill="{color}"/>"#,
        bx + px,
        by + py,
        bx - px,
        by - py
    )
}

fn svg_open(w: f64, h: f64, bg: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0}" height="{h:.0}" viewBox="0 0 {w:.1} {h:.1}"><rect width="100%" height="100%" rx="8" fill="{bg}"/>"#
    )
}

fn escape_xml(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn strip_keyword_ci<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    if line.len() < kw.len() || !line.is_char_boundary(kw.len()) || !line[..kw.len()].eq_ignore_ascii_case(kw) {
        return None;
    }
    let rest = &line[kw.len()..];
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim_start())
}

fn split_mermaid_statements(line: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_quote = false;
    for (i, b) in line.bytes().enumerate() {
        if b == b'"' {
            in_quote = !in_quote;
        } else if b == b';' && !in_quote {
            parts.push(&line[start..i]);
            start = i + 1;
        }
    }
    parts.push(&line[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_flowchart_sequence_state_pie_and_rejects_unsupported() {
        let flow = render_mermaid_svg("flowchart LR\n  A[Client] -->|WS| B(Server)\n  B --> C{Worker & Queue}", true).unwrap();
        assert!(flow.svg.contains("<svg") && flow.svg.contains("Client") && flow.svg.contains("Worker &amp; Queue"));
        assert!(flow.width > 100.0 && flow.height > 60.0);

        let seq = render_mermaid_svg("sequenceDiagram\n  User->>App: Send prompt\n  App-->>User: Stream reply", false).unwrap();
        assert!(seq.svg.contains("User") && seq.svg.contains("Send prompt"));

        let state = render_mermaid_svg("stateDiagram-v2\n  [*] --> Ready\n  Ready --> Running : start", true).unwrap();
        assert!(state.svg.contains("Ready") && state.svg.contains("Running"));

        let pie = render_mermaid_svg("pie title Coverage\n  \"Done\" : 80\n  \"Pending\" : 20", false).unwrap();
        assert!(pie.svg.contains("Coverage") && pie.svg.contains("80.0%"));

        // Multi-byte UTF-8 landing across keyword byte lengths must not panic in `strip_keyword_ci`.
        let utf8_flow = render_mermaid_svg("flowchart LR\n  \"🎉状態\" --> \"日本語テスト\"", true).unwrap();
        assert!(utf8_flow.svg.contains("🎉状態") && utf8_flow.svg.contains("日本語テスト"));

        assert!(render_mermaid_svg("gantt\n  title Unsupported", true).is_err());
        assert!(render_mermaid_svg("", true).is_err());
    }
}
