//! Preview annotations for server-streamed browser tabs: region/point selection in page CSS
//! pixels, frame crop coordinates, and structured prompt formatting for the composer.

use super::stream::Viewport;

/// Minimum pointer drag distance in page CSS px to treat a gesture as a region box rather than
/// a point click.
pub const REGION_DRAG_MIN_PX: f64 = 12.;

/// A selected rectangle on the preview page, in page CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnnotationRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl AnnotationRegion {
    /// Builds a normalized, viewport-clamped region from drag start and end points in page CSS px.
    /// Returns `None` when the drag is smaller than `REGION_DRAG_MIN_PX` on both axes (point click).
    pub fn from_drag(start: (f64, f64), end: (f64, f64), viewport: Viewport) -> Option<Self> {
        if !(viewport.width > 0. && viewport.height > 0.) {
            return None;
        }
        let x0 = start.0.min(end.0).clamp(0., viewport.width);
        let y0 = start.1.min(end.1).clamp(0., viewport.height);
        let x1 = start.0.max(end.0).clamp(0., viewport.width);
        let y1 = start.1.max(end.1).clamp(0., viewport.height);
        let width = x1 - x0;
        let height = y1 - y0;
        if width < REGION_DRAG_MIN_PX && height < REGION_DRAG_MIN_PX {
            return None;
        }
        Some(Self { x: x0, y: y0, width: width.max(1.), height: height.max(1.) })
    }

    /// Maps the region into integer device-pixel crop bounds `(x, y, width, height)` inside a
    /// streamed frame of `frame_size`.
    pub fn frame_crop_rect(&self, frame_size: (u32, u32), viewport: Viewport) -> Option<(u32, u32, u32, u32)> {
        if frame_size.0 == 0 || frame_size.1 == 0 || !(viewport.width > 0. && viewport.height > 0.) {
            return None;
        }
        let sx = f64::from(frame_size.0) / viewport.width;
        let sy = f64::from(frame_size.1) / viewport.height;
        let left = (self.x * sx).floor().clamp(0., f64::from(frame_size.0 - 1)) as u32;
        let top = (self.y * sy).floor().clamp(0., f64::from(frame_size.1 - 1)) as u32;
        let right = ((self.x + self.width) * sx).ceil().clamp(f64::from(left + 1), f64::from(frame_size.0)) as u32;
        let bottom = ((self.y + self.height) * sy).ceil().clamp(f64::from(top + 1), f64::from(frame_size.1)) as u32;
        Some((left, top, right.saturating_sub(left).max(1), bottom.saturating_sub(top).max(1)))
    }

    pub fn summary(&self) -> String {
        format!(
            "Region {}×{} at ({}, {})",
            self.width.round() as u32,
            self.height.round() as u32,
            self.x.round() as u32,
            self.y.round() as u32
        )
    }
}

/// What the user picked on the streamed preview before adding it to the composer.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationTarget {
    Region(AnnotationRegion),
    Point { x: f64, y: f64 },
    FullPage,
}

impl AnnotationTarget {
    pub fn summary(&self) -> String {
        match self {
            AnnotationTarget::Region(region) => region.summary(),
            AnnotationTarget::Point { x, y } => format!("Point ({}, {})", x.round() as u32, y.round() as u32),
            AnnotationTarget::FullPage => "Full page".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewAnnotationDraft {
    pub page_url: String,
    pub page_title: String,
    pub target: AnnotationTarget,
    pub comment: String,
}

/// Formats an annotation draft into a concise, structured prompt block for the composer.
pub fn format_annotation_prompt(draft: &PreviewAnnotationDraft) -> String {
    let mut lines = Vec::new();
    let title = draft.page_title.trim();
    let url = draft.page_url.trim();
    if !title.is_empty() && !url.is_empty() {
        lines.push(format!("Preview annotation on \"{title}\" ({url}) — {}:", draft.target.summary()));
    } else if !url.is_empty() {
        lines.push(format!("Preview annotation on {url} — {}:", draft.target.summary()));
    } else {
        lines.push(format!("Preview annotation — {}:", draft.target.summary()));
    }
    let comment = draft.comment.trim();
    if !comment.is_empty() {
        lines.push(comment.to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_from_drag_normalizes_and_maps_to_frame_crop_rect() {
        let viewport = Viewport { width: 800., height: 600. };
        // Small clicks below threshold become None (point click).
        assert_eq!(AnnotationRegion::from_drag((100., 100.), (105., 106.), viewport), None);
        // Reversed drag normalizes and clamps to viewport.
        let region = AnnotationRegion::from_drag((420., 350.), (-20., 50.), viewport).unwrap();
        assert_eq!(region, AnnotationRegion { x: 0., y: 50., width: 420., height: 300. });
        assert_eq!(region.summary(), "Region 420×300 at (0, 50)");
        // 2x HiDPI frame maps 1:2 cleanly.
        assert_eq!(region.frame_crop_rect((1600, 1200), viewport), Some((0, 100, 840, 600)));
    }

    #[test]
    fn formats_annotation_prompt_with_url_title_target_and_comment() {
        let prompt = format_annotation_prompt(&PreviewAnnotationDraft {
            page_url: "http://localhost:5173/dashboard".into(),
            page_title: "Dashboard".into(),
            target: AnnotationTarget::Region(AnnotationRegion { x: 20., y: 40., width: 320., height: 180. }),
            comment: "Align the header buttons with the card grid.".into(),
        });
        assert_eq!(
            prompt,
            "Preview annotation on \"Dashboard\" (http://localhost:5173/dashboard) — Region 320×180 at (20, 40):\nAlign the header buttons with the card grid."
        );
    }
}
