//! Lucide icons vendored at the web's version (`lucide-react` 0.564.0) with the web's stroke
//! baked in (design system §8): 1.75 by default, 2 under `stroke-2/` for work-row icons. The
//! composer's own glyphs (send arrow, stop square, filled star and bolt) live under `composer/`.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, Pixels, SharedString, Svg, prelude::*, svg};

macro_rules! icons {
    ($($variant:ident => $path:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Icon {
            $($variant),*
        }

        impl Icon {
            pub fn path(self) -> &'static str {
                match self {
                    $(Icon::$variant => $path),*
                }
            }
        }

        const FILES: &[(&str, &[u8])] = &[
            $(($path, include_bytes!(concat!("../assets/", $path)))),*
        ];
    };
}

icons! {
    ArrowRightLeft => "icons/lucide/arrow-right-left.svg",
    ArrowUp => "icons/lucide/arrow-up.svg",
    Bot => "icons/lucide/bot.svg",
    Check => "icons/lucide/check.svg",
    ChevronDown => "icons/lucide/chevron-down.svg",
    ChevronRight => "icons/lucide/chevron-right.svg",
    ChevronUp => "icons/lucide/chevron-up.svg",
    CircleAlert => "icons/lucide/circle-alert.svg",
    CircleStop => "icons/lucide/circle-stop.svg",
    Copy => "icons/lucide/copy.svg",
    CornerUpRight => "icons/lucide/corner-up-right.svg",
    Ellipsis => "icons/lucide/ellipsis.svg",
    Eye => "icons/lucide/eye.svg",
    Folder => "icons/lucide/folder.svg",
    GitFork => "icons/lucide/git-fork.svg",
    Hammer => "icons/lucide/hammer.svg",
    Link => "icons/lucide/link.svg",
    ListPlus => "icons/lucide/list-plus.svg",
    Lock => "icons/lucide/lock.svg",
    MessageSquare => "icons/lucide/message-square.svg",
    Minimize2 => "icons/lucide/minimize-2.svg",
    PenLine => "icons/lucide/pen-line.svg",
    PencilRuler => "icons/lucide/pencil-ruler.svg",
    Redo2 => "icons/lucide/redo-2.svg",
    Search => "icons/lucide/search.svg",
    Shield => "icons/lucide/shield.svg",
    ShieldOff => "icons/lucide/shield-off.svg",
    Sparkles => "icons/lucide/sparkles.svg",
    SquarePen => "icons/lucide/square-pen.svg",
    Star => "icons/lucide/star.svg",
    Terminal => "icons/lucide/terminal.svg",
    TriangleAlert => "icons/lucide/triangle-alert.svg",
    Wrench => "icons/lucide/wrench.svg",
    X => "icons/lucide/x.svg",
    WorkBot => "icons/lucide/stroke-2/bot.svg",
    WorkBrain => "icons/lucide/stroke-2/brain.svg",
    WorkCheck => "icons/lucide/stroke-2/check.svg",
    WorkCircleAlert => "icons/lucide/stroke-2/circle-alert.svg",
    WorkCode => "icons/lucide/stroke-2/code-xml.svg",
    WorkEye => "icons/lucide/stroke-2/eye.svg",
    WorkGlobe => "icons/lucide/stroke-2/globe.svg",
    WorkHammer => "icons/lucide/stroke-2/hammer.svg",
    WorkListTodo => "icons/lucide/stroke-2/list-todo.svg",
    WorkMessageCircle => "icons/lucide/stroke-2/message-circle.svg",
    WorkMessageSquare => "icons/lucide/stroke-2/message-square.svg",
    WorkMonitor => "icons/lucide/stroke-2/monitor.svg",
    WorkSearch => "icons/lucide/stroke-2/search.svg",
    WorkSquarePen => "icons/lucide/stroke-2/square-pen.svg",
    WorkTerminal => "icons/lucide/stroke-2/terminal.svg",
    WorkWrench => "icons/lucide/stroke-2/wrench.svg",
    WorkX => "icons/lucide/stroke-2/x.svg",
    WorkZap => "icons/lucide/stroke-2/zap.svg",
    Send => "icons/composer/send.svg",
    Stop => "icons/composer/stop.svg",
    StarFill => "icons/composer/star-fill.svg",
    ZapFill => "icons/composer/zap-fill.svg",
}

impl Icon {
    /// A monochrome icon of `size` in `color`.
    pub fn render(self, size: Pixels, color: Hsla) -> Svg {
        svg().path(self.path()).size(size).flex_none().text_color(color)
    }
}

/// Serves the vendored icons to GPUI.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(FILES.iter().find(|(candidate, _)| *candidate == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(FILES.iter().filter(|(candidate, _)| candidate.starts_with(path)).map(|(candidate, _)| (*candidate).into()).collect())
    }
}
