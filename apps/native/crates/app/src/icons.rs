//! Lucide icons vendored at the web's version (`lucide-react` 0.564.0) with the web's stroke
//! baked in (design system §8): 1.75 by default, 2 under `stroke-2/` for work-row icons. The
//! composer's own glyphs (send arrow, stop square, filled star and bolt) live under `composer/`,
//! provider brand marks under `providers/`, and the preview browser's own marks under `preview/`.
//! The asset source also serves gpui-component's own icons, which its `IconName` widgets load.

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
    Rocket => "icons/lucide/rocket.svg",
    Building => "icons/lucide/building.svg",
    AlarmClock => "icons/lucide/alarm-clock.svg",
    AlarmClockOff => "icons/lucide/alarm-clock-off.svg",
    Archive => "icons/lucide/archive.svg",
    ArchiveX => "icons/lucide/archive-x.svg",
    ArrowDown => "icons/lucide/arrow-down.svg",
    ArrowDownUp => "icons/lucide/arrow-down-up.svg",
    ArrowLeft => "icons/lucide/arrow-left.svg",
    ArrowRight => "icons/lucide/arrow-right.svg",
    ArrowUpLeft => "icons/lucide/arrow-up-left.svg",
    ArrowRightLeft => "icons/lucide/arrow-right-left.svg",
    ArrowUp => "icons/lucide/arrow-up.svg",
    ArrowUpRight => "icons/lucide/arrow-up-right.svg",
    Blocks => "icons/lucide/blocks.svg",
    Bookmark => "icons/lucide/bookmark.svg",
    Bot => "icons/lucide/bot.svg",
    Bug => "icons/lucide/bug.svg",
    CalendarClock => "icons/lucide/calendar-clock.svg",
    ChartNoAxesColumn => "icons/lucide/chart-no-axes-column.svg",
    Check => "icons/lucide/check.svg",
    ChevronDown => "icons/lucide/chevron-down.svg",
    ChevronLeft => "icons/lucide/chevron-left.svg",
    ChevronRight => "icons/lucide/chevron-right.svg",
    ChevronUp => "icons/lucide/chevron-up.svg",
    ChevronsDownUp => "icons/lucide/chevrons-down-up.svg",
    ChevronsLeftRightEllipsis => "icons/lucide/chevrons-left-right-ellipsis.svg",
    ChevronsUpDown => "icons/lucide/chevrons-up-down.svg",
    CircleAlert => "icons/lucide/circle-alert.svg",
    CircleArrowUp => "icons/lucide/circle-arrow-up.svg",
    CircleCheck => "icons/lucide/circle-check.svg",
    CircleDashed => "icons/lucide/circle-dashed.svg",
    CircleDot => "icons/lucide/circle-dot.svg",
    CircleDotDashed => "icons/lucide/circle-dot-dashed.svg",
    CircleSlash => "icons/lucide/circle-slash.svg",
    CircleStop => "icons/lucide/circle-stop.svg",
    CircleX => "icons/lucide/circle-x.svg",
    Clock => "icons/lucide/clock.svg",
    MousePointerClick => "icons/lucide/mouse-pointer-click.svg",
    Cloud => "icons/lucide/cloud.svg",
    Linux => "icons/environment/linux.svg",
    MacMini => "icons/environment/mac-mini.svg",
    MacStudio => "icons/environment/mac-studio.svg",
    CloudDownload => "icons/lucide/cloud-download.svg",
    CloudUpload => "icons/lucide/cloud-upload.svg",
    Columns2 => "icons/lucide/columns-2.svg",
    CodeXml => "icons/lucide/code-xml.svg",
    Copy => "icons/lucide/copy.svg",
    CornerLeftUp => "icons/lucide/corner-left-up.svg",
    CornerUpRight => "icons/lucide/corner-up-right.svg",
    Download => "icons/lucide/download.svg",
    Earth => "icons/lucide/earth.svg",
    Ellipsis => "icons/lucide/ellipsis.svg",
    ExternalLink => "icons/lucide/external-link.svg",
    Eye => "icons/lucide/eye.svg",
    File => "icons/lucide/file.svg",
    FileBraces => "icons/lucide/file-braces.svg",
    FileDiff => "icons/lucide/file-diff.svg",
    FileMinus => "icons/lucide/file-minus.svg",
    FilePlus => "icons/lucide/file-plus.svg",
    FileSymlink => "icons/lucide/file-symlink.svg",
    FileText => "icons/lucide/file-text.svg",
    Files => "icons/lucide/files.svg",
    FlaskConical => "icons/lucide/flask-conical.svg",
    FolderCode => "icons/lucide/folder-code.svg",
    Folder => "icons/lucide/folder.svg",
    FolderOpen => "icons/lucide/folder-open.svg",
    FolderGit => "icons/lucide/folder-git.svg",
    FolderGit2 => "icons/lucide/folder-git-2.svg",
    FolderPlus => "icons/lucide/folder-plus.svg",
    FolderTree => "icons/lucide/folder-tree.svg",
    Gauge => "icons/lucide/gauge.svg",
    Globe => "icons/lucide/globe.svg",
    GitBranch => "icons/lucide/git-branch.svg",
    GitBranchPlus => "icons/lucide/git-branch-plus.svg",
    GitCommit => "icons/lucide/git-commit-horizontal.svg",
    GitFork => "icons/lucide/git-fork.svg",
    GitMerge => "icons/lucide/git-merge.svg",
    GitPullRequest => "icons/lucide/git-pull-request.svg",
    GitPullRequestArrow => "icons/lucide/git-pull-request-arrow.svg",
    GitPullRequestClosed => "icons/lucide/git-pull-request-closed.svg",
    GitPullRequestDraft => "icons/lucide/git-pull-request-draft.svg",
    Hammer => "icons/lucide/hammer.svg",
    Hash => "icons/lucide/hash.svg",
    History => "icons/lucide/history.svg",
    House => "icons/lucide/house.svg",
    Image => "icons/lucide/image.svg",
    Info => "icons/lucide/info.svg",
    KeyRound => "icons/lucide/key-round.svg",
    Keyboard => "icons/lucide/keyboard.svg",
    Laptop => "icons/lucide/laptop.svg",
    Link => "icons/lucide/link.svg",
    Link2 => "icons/lucide/link-2.svg",
    ListChecks => "icons/lucide/list-checks.svg",
    ListFilter => "icons/lucide/list-filter.svg",
    ListPlus => "icons/lucide/list-plus.svg",
    ListTree => "icons/lucide/list-tree.svg",
    LoaderCircle => "icons/lucide/loader-circle.svg",
    Lock => "icons/lucide/lock.svg",
    LockOpen => "icons/lucide/lock-open.svg",
    MailOpen => "icons/lucide/mail-open.svg",
    Maximize2 => "icons/lucide/maximize-2.svg",
    MessageCircle => "icons/lucide/message-circle.svg",
    MessageCircleQuestion => "icons/lucide/message-circle-question-mark.svg",
    MessageSquare => "icons/lucide/message-square.svg",
    MessageSquareDashed => "icons/lucide/message-square-dashed.svg",
    MessageSquarePlus => "icons/lucide/message-square-plus.svg",
    MessageSquareText => "icons/lucide/message-square-text.svg",
    MessagesSquare => "icons/lucide/messages-square.svg",
    Minimize2 => "icons/lucide/minimize-2.svg",
    Minus => "icons/lucide/minus.svg",
    Monitor => "icons/lucide/monitor.svg",
    Package => "icons/lucide/package.svg",
    Palette => "icons/lucide/palette.svg",
    PanelLeft => "icons/lucide/panel-left.svg",
    PanelRight => "icons/lucide/panel-right.svg",
    PanelsTopLeft => "icons/lucide/panels-top-left.svg",
    Paperclip => "icons/lucide/paperclip.svg",
    PenLine => "icons/lucide/pen-line.svg",
    Quote => "icons/lucide/quote.svg",
    Pencil => "icons/lucide/pencil.svg",
    PencilRuler => "icons/lucide/pencil-ruler.svg",
    Pilcrow => "icons/lucide/pilcrow.svg",
    Pin => "icons/lucide/pin.svg",
    PinOff => "icons/lucide/pin-off.svg",
    Play => "icons/lucide/play.svg",
    Plus => "icons/lucide/plus.svg",
    Power => "icons/lucide/power.svg",
    RadioTower => "icons/lucide/radio-tower.svg",
    Redo2 => "icons/lucide/redo-2.svg",
    RefreshCw => "icons/lucide/refresh-cw.svg",
    RotateCcw => "icons/lucide/rotate-ccw.svg",
    QrCode => "icons/lucide/qr-code.svg",
    Rows3 => "icons/lucide/rows-3.svg",
    Search => "icons/lucide/search.svg",
    Send => "icons/composer/send.svg",
    Server => "icons/lucide/server.svg",
    Settings => "icons/lucide/settings.svg",
    Settings2 => "icons/lucide/settings-2.svg",
    Shield => "icons/lucide/shield.svg",
    ShieldCheck => "icons/lucide/shield-check.svg",
    ShieldOff => "icons/lucide/shield-off.svg",
    ShieldQuestion => "icons/lucide/shield-question-mark.svg",
    SlidersHorizontal => "icons/lucide/sliders-horizontal.svg",
    Smartphone => "icons/lucide/smartphone.svg",
    Sparkles => "icons/lucide/sparkles.svg",
    Square => "icons/lucide/square.svg",
    SquareArrowOutUpRight => "icons/lucide/square-arrow-out-up-right.svg",
    SquareMenu => "icons/lucide/square-menu.svg",
    SquarePen => "icons/lucide/square-pen.svg",
    SquareSplitHorizontal => "icons/lucide/square-split-horizontal.svg",
    SquareSplitVertical => "icons/lucide/square-split-vertical.svg",
    SquareTerminal => "icons/lucide/square-terminal.svg",
    Star => "icons/lucide/star.svg",
    StarFill => "icons/composer/star-fill.svg",
    Stop => "icons/composer/stop.svg",
    Tag => "icons/lucide/tag.svg",
    Target => "icons/lucide/target.svg",
    Terminal => "icons/lucide/terminal.svg",
    TextSearch => "icons/lucide/text-search.svg",
    TextWrap => "icons/lucide/text-wrap.svg",
    Ticket => "icons/lucide/ticket.svg",
    Timer => "icons/lucide/timer.svg",
    Trash => "icons/lucide/trash.svg",
    Trash2 => "icons/lucide/trash-2.svg",
    TrendingDown => "icons/lucide/trending-down.svg",
    TrendingUp => "icons/lucide/trending-up.svg",
    TriangleAlert => "icons/lucide/triangle-alert.svg",
    Undo2 => "icons/lucide/undo-2.svg",
    Unlink2 => "icons/lucide/unlink-2.svg",
    UserCheck => "icons/lucide/user-check.svg",
    UserPlus => "icons/lucide/user-plus.svg",
    UserRound => "icons/lucide/user-round.svg",
    UserRoundX => "icons/lucide/user-round-x.svg",
    Users => "icons/lucide/users.svg",
    Wrench => "icons/lucide/wrench.svg",
    X => "icons/lucide/x.svg",
    ZapFill => "icons/composer/zap-fill.svg",
    WorkBot => "icons/lucide/stroke-2/bot.svg",
    WorkBrain => "icons/lucide/stroke-2/brain.svg",
    WorkCheck => "icons/lucide/stroke-2/check.svg",
    WorkCircle => "icons/lucide/stroke-2/circle.svg",
    WorkCircleAlert => "icons/lucide/stroke-2/circle-alert.svg",
    WorkCode => "icons/lucide/stroke-2/code-xml.svg",
    WorkEye => "icons/lucide/stroke-2/eye.svg",
    WorkGlobe => "icons/lucide/stroke-2/globe.svg",
    WorkHammer => "icons/lucide/stroke-2/hammer.svg",
    WorkListTodo => "icons/lucide/stroke-2/list-todo.svg",
    WorkMessageCircle => "icons/lucide/stroke-2/message-circle.svg",
    WorkMessageSquare => "icons/lucide/stroke-2/message-square.svg",
    WorkMinus => "icons/lucide/stroke-2/minus.svg",
    WorkMonitor => "icons/lucide/stroke-2/monitor.svg",
    WorkSearch => "icons/lucide/stroke-2/search.svg",
    WorkSquarePen => "icons/lucide/stroke-2/square-pen.svg",
    WorkTerminal => "icons/lucide/stroke-2/terminal.svg",
    WorkWrench => "icons/lucide/stroke-2/wrench.svg",
    WorkX => "icons/lucide/stroke-2/x.svg",
    WorkZap => "icons/lucide/stroke-2/zap.svg",
    AgentPointer => "icons/lucide/stroke-2/mouse-pointer-2.svg",
    AgentPointerFill => "icons/preview/mouse-pointer-2-fill.svg",
    PreviewError => "icons/preview/error.svg",
    OnePassword => "icons/providers/one-password.svg",
    OpenAi => "icons/providers/codex.svg",
}

impl Icon {
    /// The vendored default-stroke subset; unshipped dynamic names use ProjectFavicon's folder-code fallback.
    pub fn project_named(name: &str) -> Option<Self> {
        let name = match name { "alert-circle" => "circle-alert", "check-circle" => "circle-check", _ => name };
        Self::ALL.iter().copied().find(|icon| {
            icon.path().strip_prefix("icons/lucide/").and_then(|path| path.strip_suffix(".svg")) == Some(name)
        })
    }

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
    themed: ["cursor", "grok", "fx", "kiro", "opencode", "muse", "pi"],
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
