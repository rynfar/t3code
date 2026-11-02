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
            const ALL: &[Icon] = &[$(Icon::$variant),*];

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
    AlarmClock => "icons/lucide/alarm-clock.svg",
    AlarmClockOff => "icons/lucide/alarm-clock-off.svg",
    Archive => "icons/lucide/archive.svg",
    ArrowRightLeft => "icons/lucide/arrow-right-left.svg",
    ArrowUp => "icons/lucide/arrow-up.svg",
    Bot => "icons/lucide/bot.svg",
    Check => "icons/lucide/check.svg",
    ChevronDown => "icons/lucide/chevron-down.svg",
    ChevronRight => "icons/lucide/chevron-right.svg",
    ChevronUp => "icons/lucide/chevron-up.svg",
    CircleAlert => "icons/lucide/circle-alert.svg",
    CircleCheck => "icons/lucide/circle-check.svg",
    CircleDashed => "icons/lucide/circle-dashed.svg",
    CircleDotDashed => "icons/lucide/circle-dot-dashed.svg",
    CircleStop => "icons/lucide/circle-stop.svg",
    Clock => "icons/lucide/clock.svg",
    Copy => "icons/lucide/copy.svg",
    CornerUpRight => "icons/lucide/corner-up-right.svg",
    Ellipsis => "icons/lucide/ellipsis.svg",
    Eye => "icons/lucide/eye.svg",
    Folder => "icons/lucide/folder.svg",
    FolderTree => "icons/lucide/folder-tree.svg",
    GitBranch => "icons/lucide/git-branch.svg",
    GitFork => "icons/lucide/git-fork.svg",
    Hammer => "icons/lucide/hammer.svg",
    Hash => "icons/lucide/hash.svg",
    Link => "icons/lucide/link.svg",
    ListPlus => "icons/lucide/list-plus.svg",
    Lock => "icons/lucide/lock.svg",
    MailOpen => "icons/lucide/mail-open.svg",
    MessageCircleQuestion => "icons/lucide/message-circle-question-mark.svg",
    MessageSquare => "icons/lucide/message-square.svg",
    MessageSquarePlus => "icons/lucide/message-square-plus.svg",
    Minimize2 => "icons/lucide/minimize-2.svg",
    PanelLeft => "icons/lucide/panel-left.svg",
    PenLine => "icons/lucide/pen-line.svg",
    Pencil => "icons/lucide/pencil.svg",
    PencilRuler => "icons/lucide/pencil-ruler.svg",
    Pin => "icons/lucide/pin.svg",
    PinOff => "icons/lucide/pin-off.svg",
    Redo2 => "icons/lucide/redo-2.svg",
    RefreshCw => "icons/lucide/refresh-cw.svg",
    Search => "icons/lucide/search.svg",
    Settings => "icons/lucide/settings.svg",
    Shield => "icons/lucide/shield.svg",
    ShieldOff => "icons/lucide/shield-off.svg",
    ShieldQuestion => "icons/lucide/shield-question-mark.svg",
    Sparkles => "icons/lucide/sparkles.svg",
    SquarePen => "icons/lucide/square-pen.svg",
    Star => "icons/lucide/star.svg",
    Terminal => "icons/lucide/terminal.svg",
    TextSearch => "icons/lucide/text-search.svg",
    Timer => "icons/lucide/timer.svg",
    Trash => "icons/lucide/trash.svg",
    TriangleAlert => "icons/lucide/triangle-alert.svg",
    Undo2 => "icons/lucide/undo-2.svg",
    UserRound => "icons/lucide/user-round.svg",
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
        self.sized(size).text_color(color)
    }

    /// An icon of `size` in the inherited text color, so a parent's hover color reaches it.
    pub fn sized(self, size: Pixels) -> Svg {
        svg().path(self.path()).size(size).flex_none()
    }

    /// The 1.75-stroke icon for a Lucide name, as menu models name icons.
    pub fn from_name(name: &str) -> Option<Icon> {
        Self::ALL.iter().copied().find(|icon| icon.path().strip_prefix("icons/lucide/").and_then(|file| file.strip_suffix(".svg")) == Some(name))
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
