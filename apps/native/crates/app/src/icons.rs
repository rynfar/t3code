//! Lucide icons vendored at the web's version (`lucide-react` 0.564.0) with the web's stroke
//! baked in (design system §8): 1.75 by default, 2 under `stroke-2/` for work-row icons.

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
    Check => "icons/lucide/check.svg",
    ChevronDown => "icons/lucide/chevron-down.svg",
    ChevronRight => "icons/lucide/chevron-right.svg",
    CircleAlert => "icons/lucide/circle-alert.svg",
    CircleStop => "icons/lucide/circle-stop.svg",
    Copy => "icons/lucide/copy.svg",
    GitFork => "icons/lucide/git-fork.svg",
    Link => "icons/lucide/link.svg",
    MessageSquare => "icons/lucide/message-square.svg",
    Minimize2 => "icons/lucide/minimize-2.svg",
    Redo2 => "icons/lucide/redo-2.svg",
    Shield => "icons/lucide/shield.svg",
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
