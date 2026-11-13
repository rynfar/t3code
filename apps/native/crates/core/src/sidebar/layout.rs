//! Left sidebar width and presentation. Ports `apps/web/src/components/threadSidebarWidth.ts`,
//! the resize acceptance in `components/AppSidebarLayout.tsx`, and the mobile sheet breakpoint
//! and width of `components/ui/sidebar.tsx`.

/// `THREAD_SIDEBAR_WIDTH_STORAGE_KEY`: the persisted width's file in the native home.
pub const STORAGE_KEY: &str = "chat_thread_sidebar_width";
pub const DEFAULT_WIDTH: f32 = 16. * 16.;
pub const MIN_WIDTH: f32 = 13. * 16.;
/// `THREAD_MAIN_CONTENT_MIN_WIDTH`: a resize never squeezes the thread below this.
pub const MAIN_CONTENT_MIN_WIDTH: f32 = 40. * 16.;
/// `SidebarRail` hit target, centred on the sidebar's right edge.
pub const RAIL_WIDTH: f32 = 16.;
/// `SIDEBAR_MOBILE_QUERY` is `(max-width: 767px)`: below this the sidebar is a sheet.
pub const SHEET_BREAKPOINT: f32 = 768.;
/// `SIDEBAR_WIDTH_MOBILE` is `calc(100vw - var(--spacing(3)))`.
pub const SHEET_INSET: f32 = 12.;

pub fn minimum_width(brand_width: f32) -> f32 {
    MIN_WIDTH.max(brand_width.ceil())
}

pub fn maximum_width(viewport_width: f32, minimum_width: f32) -> f32 {
    minimum_width.max(viewport_width.floor() - MAIN_CONTENT_MIN_WIDTH)
}

pub fn clamp_width(width: f32, minimum_width: f32, maximum_width: f32) -> f32 {
    maximum_width.min(minimum_width.max(width))
}

/// The stored width floored at the minimum, or the default when nothing usable is stored.
pub fn preferred_width(stored: Option<f32>) -> f32 {
    stored.filter(|width| width.is_finite()).map_or(DEFAULT_WIDTH, |width| MIN_WIDTH.max(width))
}

/// `resolveInitialThreadSidebarWidth`: the preferred width capped so the thread keeps its
/// minimum width.
pub fn initial_width(stored: Option<f32>, viewport_width: f32) -> f32 {
    preferred_width(stored).min(maximum_width(viewport_width, MIN_WIDTH))
}

/// The width the docked sidebar renders at for `width` in this window.
pub fn resolved_width(width: f32, viewport_width: f32, brand_width: f32) -> f32 {
    let minimum = minimum_width(brand_width);
    clamp_width(width, minimum, maximum_width(viewport_width, minimum))
}

/// `shouldAcceptWidth`: shrinking is always allowed; growing only while the thread keeps its
/// minimum width.
pub fn accepts_width(current_width: f32, next_width: f32, viewport_width: f32) -> bool {
    next_width <= current_width || viewport_width - next_width >= MAIN_CONTENT_MIN_WIDTH
}

/// One drag step: the pointer's distance from the window's left edge becomes the width, clamped,
/// and dropped when it would squeeze the thread.
pub fn drag_width(current_width: f32, pointer_x: f32, viewport_width: f32, brand_width: f32) -> f32 {
    let next = resolved_width(pointer_x, viewport_width, brand_width);
    if accepts_width(current_width, next, viewport_width) { next } else { current_width }
}

pub fn is_sheet(viewport_width: f32) -> bool {
    viewport_width < SHEET_BREAKPOINT
}

pub fn sheet_width(viewport_width: f32) -> f32 {
    (viewport_width - SHEET_INSET).max(0.)
}

/// How much of the window row the sidebar occupies: nothing while collapsed or shown as a sheet.
pub fn inline_width(width: f32, collapsed: bool, viewport_width: f32, brand_width: f32) -> f32 {
    if collapsed || is_sheet(viewport_width) { 0. } else { resolved_width(width, viewport_width, brand_width) }
}

/// The narrow-window sheet (`openMobile`), kept apart from the docked `collapsed` flag. Widening
/// past the breakpoint closes it, so narrowing again never pops a stale sheet open.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SidebarSheet {
    open: bool,
    viewport_width: f32,
}

impl Default for SidebarSheet {
    fn default() -> Self {
        SidebarSheet { open: false, viewport_width: f32::INFINITY }
    }
}

impl SidebarSheet {
    pub fn is_sheet(&self) -> bool {
        is_sheet(self.viewport_width)
    }

    pub fn is_open(&self) -> bool {
        self.open && self.is_sheet()
    }

    /// Whether the sidebar is out of sight: the closed sheet below the breakpoint, else `collapsed`.
    pub fn hidden(&self, collapsed: bool) -> bool {
        if self.is_sheet() { !self.open } else { collapsed }
    }

    /// Returns whether the window crossed the breakpoint.
    pub fn set_viewport(&mut self, viewport_width: f32) -> bool {
        let was_sheet = self.is_sheet();
        self.viewport_width = viewport_width;
        if !self.is_sheet() {
            self.open = false;
        }
        was_sheet != self.is_sheet()
    }

    /// `toggleSidebar` below the breakpoint; `false` leaves the docked sidebar to the caller.
    pub fn toggle(&mut self) -> bool {
        if !self.is_sheet() {
            return false;
        }
        self.open = !self.open;
        true
    }

    /// Returns whether a visible sheet closed.
    pub fn close(&mut self) -> bool {
        std::mem::take(&mut self.open) && self.is_sheet()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimum_grows_with_the_brand() {
        assert_eq!(minimum_width(0.), 208.);
        assert_eq!(minimum_width(230.2), 231.);
    }

    #[test]
    fn maximum_leaves_the_thread_its_minimum() {
        assert_eq!(maximum_width(1440., MIN_WIDTH), 800.);
        assert_eq!(maximum_width(1440.9, MIN_WIDTH), 800.);
        assert_eq!(maximum_width(800., MIN_WIDTH), 208.);
        assert_eq!(maximum_width(800., 231.), 231.);
    }

    #[test]
    fn initial_width_uses_stored_or_default_within_the_window() {
        assert_eq!(initial_width(None, 1440.), 256.);
        assert_eq!(initial_width(Some(320.), 1440.), 320.);
        assert_eq!(initial_width(Some(100.), 1440.), 208.);
        assert_eq!(initial_width(Some(900.), 1440.), 800.);
        assert_eq!(initial_width(None, 860.), 220.);
        assert_eq!(initial_width(Some(f32::NAN), 1440.), 256.);
    }

    #[test]
    fn resolved_width_clamps_to_both_bounds() {
        assert_eq!(resolved_width(150., 1440., 0.), 208.);
        assert_eq!(resolved_width(300., 1440., 0.), 300.);
        assert_eq!(resolved_width(1000., 1440., 0.), 800.);
        assert_eq!(resolved_width(256., 800., 0.), 208.);
    }

    #[test]
    fn drag_grows_only_while_the_thread_fits() {
        assert_eq!(drag_width(256., 400., 1440., 0.), 400.);
        assert_eq!(drag_width(256., 1200., 1440., 0.), 800.);
        assert_eq!(drag_width(400., 100., 1440., 0.), 208.);
        // A window narrower than minimum + thread: shrinking still works, growing does not.
        assert!(accepts_width(260., 208., 700.));
        assert!(!accepts_width(208., 220., 700.));
    }

    #[test]
    fn reset_returns_to_the_default() {
        assert_eq!(preferred_width(None), DEFAULT_WIDTH);
        assert_eq!(preferred_width(Some(f32::INFINITY)), DEFAULT_WIDTH);
        assert_eq!(preferred_width(Some(120.)), MIN_WIDTH);
        assert_eq!(initial_width(None, 1440.), DEFAULT_WIDTH);
    }

    #[test]
    fn narrow_windows_show_a_sheet_that_takes_no_room() {
        assert!(is_sheet(767.));
        assert!(!is_sheet(768.));
        assert_eq!(sheet_width(767.), 755.);
        assert_eq!(sheet_width(4.), 0.);
        assert_eq!(inline_width(300., false, 767., 0.), 0.);
        assert_eq!(inline_width(300., true, 1440., 0.), 0.);
        assert_eq!(inline_width(300., false, 1440., 0.), 300.);
    }

    #[test]
    fn sheet_toggles_only_below_the_breakpoint() {
        let mut sheet = SidebarSheet::default();
        assert!(!sheet.toggle());
        assert!(!sheet.hidden(false));
        assert!(sheet.hidden(true));
        assert!(sheet.set_viewport(700.));
        assert!(sheet.hidden(false));
        assert!(sheet.toggle());
        assert!(sheet.is_open());
        assert!(!sheet.hidden(true));
        assert!(sheet.close());
        assert!(!sheet.close());
        assert!(!sheet.is_open());
    }

    #[test]
    fn widening_closes_the_sheet_for_good() {
        let mut sheet = SidebarSheet::default();
        sheet.set_viewport(700.);
        sheet.toggle();
        assert!(!sheet.set_viewport(720.));
        assert!(sheet.is_open());
        assert!(sheet.set_viewport(1024.));
        assert!(!sheet.is_open());
        assert!(!sheet.close());
        sheet.set_viewport(700.);
        assert!(!sheet.is_open());
    }
}
