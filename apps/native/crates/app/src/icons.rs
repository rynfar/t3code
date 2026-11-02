//! Lucide icons vendored at the web's version (`lucide-react` 0.564.0) with the web's stroke
//! baked in (design system §8): 1.75 by default, 2 under `stroke-2/` for work-row icons. The
//! composer's own glyphs (send arrow, stop square, filled star and bolt) live under `composer/`,
//! and provider brand marks under `providers/`. The asset source also serves gpui-component's own
//! icons, which its `IconName` widgets load.

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
    ArchiveX => "icons/lucide/archive-x.svg",
    ArrowRightLeft => "icons/lucide/arrow-right-left.svg",
    ArrowUp => "icons/lucide/arrow-up.svg",
    Bot => "icons/lucide/bot.svg",
    Check => "icons/lucide/check.svg",
    ChevronDown => "icons/lucide/chevron-down.svg",
    ChevronRight => "icons/lucide/chevron-right.svg",
    ChevronUp => "icons/lucide/chevron-up.svg",
    ChevronsDownUp => "icons/lucide/chevrons-down-up.svg",
    ChevronsUpDown => "icons/lucide/chevrons-up-down.svg",
    CircleAlert => "icons/lucide/circle-alert.svg",
    CircleCheck => "icons/lucide/circle-check.svg",
    CircleDashed => "icons/lucide/circle-dashed.svg",
    CircleDotDashed => "icons/lucide/circle-dot-dashed.svg",
    CircleStop => "icons/lucide/circle-stop.svg",
    Clock => "icons/lucide/clock.svg",
    Columns2 => "icons/lucide/columns-2.svg",
    Copy => "icons/lucide/copy.svg",
    CornerUpRight => "icons/lucide/corner-up-right.svg",
    Ellipsis => "icons/lucide/ellipsis.svg",
    Eye => "icons/lucide/eye.svg",
    FileDiff => "icons/lucide/file-diff.svg",
    FileMinus => "icons/lucide/file-minus.svg",
    FilePlus => "icons/lucide/file-plus.svg",
    FileSymlink => "icons/lucide/file-symlink.svg",
    FlaskConical => "icons/lucide/flask-conical.svg",
    Folder => "icons/lucide/folder.svg",
    FolderPlus => "icons/lucide/folder-plus.svg",
    FolderTree => "icons/lucide/folder-tree.svg",
    GitBranch => "icons/lucide/git-branch.svg",
    GitFork => "icons/lucide/git-fork.svg",
    Hammer => "icons/lucide/hammer.svg",
    Hash => "icons/lucide/hash.svg",
    Info => "icons/lucide/info.svg",
    Keyboard => "icons/lucide/keyboard.svg",
    Link => "icons/lucide/link.svg",
    ListPlus => "icons/lucide/list-plus.svg",
    Lock => "icons/lucide/lock.svg",
    MailOpen => "icons/lucide/mail-open.svg",
    Maximize2 => "icons/lucide/maximize-2.svg",
    MessageCircleQuestion => "icons/lucide/message-circle-question-mark.svg",
    MessageSquare => "icons/lucide/message-square.svg",
    MessageSquarePlus => "icons/lucide/message-square-plus.svg",
    MessageSquareText => "icons/lucide/message-square-text.svg",
    MessagesSquare => "icons/lucide/messages-square.svg",
    Minimize2 => "icons/lucide/minimize-2.svg",
    Minus => "icons/lucide/minus.svg",
    Palette => "icons/lucide/palette.svg",
    PanelLeft => "icons/lucide/panel-left.svg",
    PanelRight => "icons/lucide/panel-right.svg",
    PenLine => "icons/lucide/pen-line.svg",
    Pencil => "icons/lucide/pencil.svg",
    PencilRuler => "icons/lucide/pencil-ruler.svg",
    Pilcrow => "icons/lucide/pilcrow.svg",
    Pin => "icons/lucide/pin.svg",
    PinOff => "icons/lucide/pin-off.svg",
    Plus => "icons/lucide/plus.svg",
    Redo2 => "icons/lucide/redo-2.svg",
    RefreshCw => "icons/lucide/refresh-cw.svg",
    Rows3 => "icons/lucide/rows-3.svg",
    Search => "icons/lucide/search.svg",
    Send => "icons/composer/send.svg",
    Server => "icons/lucide/server.svg",
    Settings => "icons/lucide/settings.svg",
    Settings2 => "icons/lucide/settings-2.svg",
    Shield => "icons/lucide/shield.svg",
    ShieldOff => "icons/lucide/shield-off.svg",
    ShieldQuestion => "icons/lucide/shield-question-mark.svg",
    Sparkles => "icons/lucide/sparkles.svg",
    Square => "icons/lucide/square.svg",
    SquarePen => "icons/lucide/square-pen.svg",
    SquareSplitHorizontal => "icons/lucide/square-split-horizontal.svg",
    SquareSplitVertical => "icons/lucide/square-split-vertical.svg",
    SquareTerminal => "icons/lucide/square-terminal.svg",
    Star => "icons/lucide/star.svg",
    StarFill => "icons/composer/star-fill.svg",
    Stop => "icons/composer/stop.svg",
    Terminal => "icons/lucide/terminal.svg",
    TextSearch => "icons/lucide/text-search.svg",
    TextWrap => "icons/lucide/text-wrap.svg",
    Timer => "icons/lucide/timer.svg",
    Trash => "icons/lucide/trash.svg",
    Trash2 => "icons/lucide/trash-2.svg",
    TriangleAlert => "icons/lucide/triangle-alert.svg",
    Undo2 => "icons/lucide/undo-2.svg",
    UserRound => "icons/lucide/user-round.svg",
    Wrench => "icons/lucide/wrench.svg",
    X => "icons/lucide/x.svg",
    ZapFill => "icons/composer/zap-fill.svg",
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

    /// The bare icon, for callers that size it and style hover or position themselves.
    pub fn svg(self) -> Svg {
        svg().path(self.path()).flex_none()
    }
}

/// How a provider's brand mark draws (`ProviderInstanceIcon`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderLogo {
    /// One color taken from the surrounding text, like a Lucide icon (Codex).
    Mono(&'static str),
    /// Fixed fills drawn with `img()`, one file per theme.
    Image { light: &'static str, dark: &'static str },
}

macro_rules! provider_logos {
    (mono: [$($mono:literal),*], image: [$($image:literal => $file:literal),*], themed: [$($themed:literal),*] $(,)?) => {
        const PROVIDER_FILES: &[(&str, &[u8])] = &[
            $((concat!("icons/providers/", $mono, ".svg"), include_bytes!(concat!("../assets/icons/providers/", $mono, ".svg"))),)*
            $((concat!("icons/providers/", $file), include_bytes!(concat!("../assets/icons/providers/", $file))),)*
            $(
                (concat!("icons/providers/", $themed, "-light.svg"), include_bytes!(concat!("../assets/icons/providers/", $themed, "-light.svg"))),
                (concat!("icons/providers/", $themed, "-dark.svg"), include_bytes!(concat!("../assets/icons/providers/", $themed, "-dark.svg"))),
            )*
        ];

        /// The brand mark vendored for a driver (`scripts/vendor-icons.ts --providers`), if any.
        pub fn provider_logo(driver: &str) -> Option<ProviderLogo> {
            match driver {
                $($mono => Some(ProviderLogo::Mono(concat!("icons/providers/", $mono, ".svg"))),)*
                $($image => Some(ProviderLogo::Image { light: concat!("icons/providers/", $file), dark: concat!("icons/providers/", $file) }),)*
                $($themed => Some(ProviderLogo::Image {
                    light: concat!("icons/providers/", $themed, "-light.svg"),
                    dark: concat!("icons/providers/", $themed, "-dark.svg"),
                }),)*
                _ => None,
            }
        }
    };
}

provider_logos! {
    mono: ["codex"],
    image: ["claudeAgent" => "claudeAgent.svg", "antigravity" => "antigravity.png"],
    themed: ["cursor", "grok", "fx", "opencode", "muse", "pi"],
}

/// Serves the vendored icons to GPUI.
pub struct Assets;

impl Assets {
    fn files() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
        FILES.iter().chain(PROVIDER_FILES)
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        match Self::files().find(|(candidate, _)| *candidate == path) {
            Some((_, bytes)) => Ok(Some(Cow::Borrowed(*bytes))),
            None => gpui_component_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = gpui_component_assets::Assets.list(path)?;
        paths.extend(Self::files().filter(|(candidate, _)| candidate.starts_with(path)).map(|(candidate, _)| SharedString::from(*candidate)));
        Ok(paths)
    }
}
