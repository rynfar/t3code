//! Lucide icons vendored at the web's version (`lucide-react` 0.564.0) with the web's stroke
//! baked in (design system §8): 1.75 by default, 2 under `stroke-2/` for work-row icons. The
//! composer's own glyphs (send arrow, stop square, filled star and bolt) live under `composer/`,
//! provider brand marks under `providers/`, and the preview browser's own marks under `preview/`.
//! The asset source also serves gpui-component's own icons, which its `IconName` widgets load.

use std::borrow::Cow;

use gpui::{AnyElement, AssetSource, Hsla, Length, Pixels, SharedString, Svg, img, prelude::*, svg};

macro_rules! icons {
    ($($(#[$doc:meta])* $variant:ident => $path:literal $(= $inline:literal)?),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Icon {
            $($(#[$doc])* $variant),*
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
            $(($path, icons!(@bytes $path $(= $inline)?))),*
        ];
    };
    (@bytes $path:literal = $inline:literal) => { $inline.as_bytes() };
    (@bytes $path:literal) => { include_bytes!(concat!("../assets/", $path)) };
}

icons! {
    /// Light appearance.
    Sun => "icons/lucide/sun.svg" = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"
         fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="12" cy="12" r="4"/>
        <path d="M12 2v2"/>
        <path d="M12 20v2"/>
        <path d="m4.93 4.93 1.41 1.41"/>
        <path d="m17.66 17.66 1.41 1.41"/>
        <path d="M2 12h2"/>
        <path d="M20 12h2"/>
        <path d="m6.34 17.66-1.41 1.41"/>
        <path d="m19.07 4.93-1.41 1.41"/>
    </svg>"#,
    /// Dark appearance.
    Moon => "icons/lucide/moon.svg" = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"
         fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <path d="M20.985 12.486a9 9 0 1 1-9.473-9.472c.405-.022.617.46.402.803a6 6 0 0 0 8.268 8.268c.344-.215.825-.004.803.401"/>
    </svg>"#,
    /// Balance scales.
    Scale => "icons/lucide/scale.svg" = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"
         fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <path d="M12 3v18"/>
        <path d="m19 8 3 8a5 5 0 0 1-6 0zV7"/>
        <path d="M3 7h1a17 17 0 0 0 8-2 17 17 0 0 0 8 2h1"/>
        <path d="m5 8 3 8a5 5 0 0 1-6 0zV7"/>
        <path d="M7 21h10"/>
    </svg>"#,
    /// Still capture.
    Camera => "icons/lucide/camera.svg" = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"
         fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <path d="M13.997 4a2 2 0 0 1 1.76 1.05l.486.9A2 2 0 0 0 18.003 7H20a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2h1.997a2 2 0 0 0 1.759-1.048l.489-.904A2 2 0 0 1 10.004 4z"/>
        <circle cx="12" cy="13" r="3"/>
    </svg>"#,
    /// Add a package.
    PackagePlus => "icons/lucide/package-plus.svg" = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"
         fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <path d="M16 16h6"/>
        <path d="M19 13v6"/>
        <path d="M21 10V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l2-1.14"/>
        <path d="m7.5 4.27 9 5.15"/>
        <polyline points="3.29 7 12 12 20.71 7"/>
        <line x1="12" x2="12" y1="22" y2="12"/>
    </svg>"#,
    /// Upload a file.
    Upload => "icons/lucide/upload.svg" = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"
         fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <path d="M12 3v12"/>
        <path d="m17 8-5-5-5 5"/>
        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
    </svg>"#,
    /// A directly entered text value.
    Type => "icons/lucide/type.svg" = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"
         fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
        <path d="M12 4v16"/>
        <path d="M4 7V5a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v2"/>
        <path d="M9 20h6"/>
    </svg>"#,
    /// The Apple platform mark.
    Apple => "icons/platform/apple.svg" = r#"<svg viewBox="0 0 24 24" fill="currentColor" xmlns="http://www.w3.org/2000/svg">
    <path d="M12.152 6.896c-.948 0-2.415-1.078-3.96-1.04-2.04.027-3.91 1.183-4.961 3.014-2.117 3.675-.546 9.103 1.519 12.09 1.013 1.454 2.208 3.09 3.792 3.039 1.52-.065 2.09-.987 3.935-.987 1.831 0 2.35.987 3.96.948 1.637-.026 2.676-1.48 3.676-2.948 1.156-1.688 1.636-3.325 1.662-3.415-.039-.013-3.182-1.221-3.22-4.857-.026-3.04 2.48-4.494 2.597-4.559-1.429-2.09-3.623-2.324-4.39-2.376-2-.156-3.675 1.09-4.61 1.09zM15.53 3.83c.843-1.012 1.4-2.427 1.245-3.83-1.207.052-2.662.805-3.532 1.818-.78.896-1.454 2.338-1.273 3.714 1.338.104 2.715-.688 3.559-1.701" />
  </svg>"#,
    /// The Android platform mark.
    Android => "icons/platform/android.svg" = r#"<svg viewBox="0 0 24 24" fill="currentColor" xmlns="http://www.w3.org/2000/svg">
    <path
      d="m8 3-1-2m9 2 1-2"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
    />
    <path
      fill-rule="evenodd"
      d="M5 10a7 7 0 0 1 14 0H5Zm4-4a.8.8 0 1 0 0 1.6A.8.8 0 0 0 9 6Zm6 0a.8.8 0 1 0 0 1.6A.8.8 0 0 0 15 6Z"
    />
    <path d="M5 11h14v7a2 2 0 0 1-2 2v2a1.5 1.5 0 0 1-3 0v-2h-4v2a1.5 1.5 0 0 1-3 0v-2a2 2 0 0 1-2-2v-7Z" />
    <rect x="1" y="11" width="3" height="8" rx="1.5" />
    <rect x="20" y="11" width="3" height="8" rx="1.5" />
  </svg>"#,
    /// The T3 wordmark, in a 94.3941 × 56.96 viewbox.
    T3Wordmark => "icons/brand/t3-wordmark.svg" = r#"<svg viewBox="15.5309 37 94.3941 56.96" xmlns="http://www.w3.org/2000/svg">
      <path
        d="M33.4509 93V47.56H15.5309V37H64.3309V47.56H46.4109V93H33.4509ZM86.7253 93.96C82.832 93.96 78.9653 93.4533 75.1253 92.44C71.2853 91.3733 68.032 89.88 65.3653 87.96L70.4053 78.04C72.5386 79.5867 75.0186 80.8133 77.8453 81.72C80.672 82.6267 83.5253 83.08 86.4053 83.08C89.6586 83.08 92.2186 82.44 94.0853 81.16C95.952 79.88 96.8853 78.12 96.8853 75.88C96.8853 73.7467 96.0586 72.0667 94.4053 70.84C92.752 69.6133 90.0853 69 86.4053 69H80.4853V60.44L96.0853 42.76L97.5253 47.4H68.1653V37H107.365V45.4L91.8453 63.08L85.2853 59.32H89.0453C95.9253 59.32 101.125 60.8667 104.645 63.96C108.165 67.0533 109.925 71.0267 109.925 75.88C109.925 79.0267 109.099 81.9867 107.445 84.76C105.792 87.48 103.259 89.6933 99.8453 91.4C96.432 93.1067 92.0586 93.96 86.7253 93.96Z"
        fill="currentColor"
      />
    </svg>"#,
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
    Eraser => "icons/lucide/eraser.svg",
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
    Layers => "icons/lucide/layers.svg",
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
    Mic => "icons/lucide/mic.svg",
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
    Reply => "icons/lucide/reply.svg",
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
    Workflow => "icons/lucide/workflow.svg",
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

    /// `EnvironmentMachineIcon` for `resolveEnvironmentMachineKind`: the chosen icon, else the
    /// reported machine, else a server.
    pub fn machine(config: &t3_core::server_config::ServerConfig) -> Icon {
        let kind = config.settings.environment_icon.as_deref().or(config.environment.platform.as_ref().map(|platform| platform.machine.as_str()));
        match kind.unwrap_or("server") {
            "laptop" => Icon::Laptop,
            "desktop" => Icon::Monitor,
            "cloud" => Icon::Cloud,
            "linux" => Icon::Linux,
            "mac-mini" => Icon::MacMini,
            "mac-studio" => Icon::MacStudio,
            _ => Icon::Server,
        }
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

// OpenInPicker.tsx artwork from Icons.tsx and JetBrainsIcons.tsx. GPUI rasterizes
// image assets at their intrinsic size, so constrain it independently of the viewBox.
// SourceControlIconArtwork.tsx draws the source-control-*/src/shared/icon.ts artwork.
// Azure DevOps mark via selfhst/icons (CC BY 4.0).
// https://github.com/selfhst/icons/blob/main/svg/azure-devops.svg
const BRAND_FILES: &[(&str, &[u8])] = &[
    ("icons/editors/cursor-light.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"

    viewBox="0 0 466.73 532.09"
>
    <path fill="#26251E" d="M457.43,125.94L244.42,2.96c-6.84-3.95-15.28-3.95-22.12,0L9.3,125.94c-5.75,3.32-9.3,9.46-9.3,16.11v247.99c0,6.65,3.55,12.79,9.3,16.11l213.01,122.98c6.84,3.95,15.28,3.95,22.12,0l213.01-122.98c5.75-3.32,9.3-9.46,9.3-16.11v-247.99c0-6.65-3.55-12.79-9.3-16.11h-.01ZM444.05,151.99l-205.63,356.16c-1.39,2.4-5.06,1.42-5.06-1.36v-233.21c0-4.66-2.49-8.97-6.53-11.31L24.87,145.67c-2.4-1.39-1.42-5.06,1.36-5.06h411.26c5.84,0,9.49,6.33,6.57,11.39h-.01Z" />
  </svg>"##),
    ("icons/editors/cursor-dark.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"

    viewBox="0 0 466.73 532.09"
>
    <path fill="#EDECEC" d="M457.43,125.94L244.42,2.96c-6.84-3.95-15.28-3.95-22.12,0L9.3,125.94c-5.75,3.32-9.3,9.46-9.3,16.11v247.99c0,6.65,3.55,12.79,9.3,16.11l213.01,122.98c6.84,3.95,15.28,3.95,22.12,0l213.01-122.98c5.75-3.32,9.3-9.46,9.3-16.11v-247.99c0-6.65-3.55-12.79-9.3-16.11h-.01ZM444.05,151.99l-205.63,356.16c-1.39,2.4-5.06,1.42-5.06-1.36v-233.21c0-4.66-2.49-8.97-6.53-11.31L24.87,145.67c-2.4-1.39-1.42-5.06,1.36-5.06h411.26c5.84,0,9.49,6.33,6.57,11.39h-.01Z" />
  </svg>"##),
    ("icons/editors/trae.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 24 24" fill="currentColor">
    <rect x="1" y="4" width="3" height="14" />
    <rect x="4" y="18" width="18" height="3" />
    <rect x="4" y="4" width="18" height="3" />
    <rect x="19" y="7" width="3" height="11" />
    <path d="M11 10L13 12L11 14L9 12Z" />
    <path d="M16 10L18 12L16 14L14 12Z" />
  </svg>"##),
    ("icons/editors/kiro.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 1200 1200" fill="none">
    <rect width="1200" height="1200" rx="260" fill="#9046FF" />
    <path
      d="M398.554 818.914C316.315 1001.03 491.477 1046.74 620.672 940.156C658.687 1059.66 801.052 970.473 852.234 877.795C964.787 673.567 919.318 465.357 907.64 422.374C827.637 129.443 427.623 128.946 358.8 423.865C342.651 475.544 342.402 534.18 333.458 595.051C328.986 625.86 325.507 645.488 313.83 677.785C306.873 696.424 297.68 712.819 282.773 740.645C259.915 783.881 269.604 867.113 387.87 823.883L399.051 818.914H398.554Z"
      fill="#fff"
    />
    <path
      d="M636.123 549.353C603.328 549.353 598.359 510.097 598.359 486.742C598.359 465.623 602.086 448.977 609.293 438.293C615.504 428.852 624.697 424.131 636.123 424.131C647.555 424.131 657.492 428.852 664.447 438.541C672.398 449.474 676.623 466.12 676.623 486.742C676.623 525.998 661.471 549.353 636.375 549.353H636.123Z"
      fill="#000"
    />
    <path
      d="M771.24 549.353C738.445 549.353 733.477 510.097 733.477 486.742C733.477 465.623 737.203 448.977 744.41 438.293C750.621 428.852 759.814 424.131 771.24 424.131C782.672 424.131 792.609 428.852 799.564 438.541C807.516 449.474 811.74 466.12 811.74 486.742C811.74 525.998 796.588 549.353 771.492 549.353H771.24Z"
      fill="#000"
    />
  </svg>"##),
    ("icons/editors/vscode.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  fill="none" viewBox="0 0 100 100">
      <mask id="maskId" width="100" height="100" x="0" y="0" maskUnits="userSpaceOnUse">
        <path
          fill="#fff"
          fill-rule="evenodd"
          d="M70.912 99.317a6.223 6.223 0 0 0 4.96-.19l20.589-9.907A6.25 6.25 0 0 0 100 83.587V16.413a6.25 6.25 0 0 0-3.54-5.632L75.874.874a6.226 6.226 0 0 0-7.104 1.21L29.355 38.04 12.187 25.01a4.162 4.162 0 0 0-5.318.236l-5.506 5.009a4.168 4.168 0 0 0-.004 6.162L16.247 50 1.36 63.583a4.168 4.168 0 0 0 .004 6.162l5.506 5.01a4.162 4.162 0 0 0 5.318.236l17.168-13.032L68.77 97.917a6.217 6.217 0 0 0 2.143 1.4ZM75.015 27.3 45.11 50l29.906 22.701V27.3Z"
          clip-rule="evenodd"
        />
      </mask>
      <g mask="url(#maskId)">
        <path
          fill="#0065A9"
          d="M96.461 10.796 75.857.876a6.23 6.23 0 0 0-7.107 1.207l-67.451 61.5a4.167 4.167 0 0 0 .004 6.162l5.51 5.009a4.167 4.167 0 0 0 5.32.236l81.228-61.62c2.725-2.067 6.639-.124 6.639 3.297v-.24a6.25 6.25 0 0 0-3.539-5.63Z"
        />
        <g filter="url(#topShadowFilterId)">
          <path
            fill="#007ACC"
            d="m96.461 89.204-20.604 9.92a6.229 6.229 0 0 1-7.107-1.207l-67.451-61.5a4.167 4.167 0 0 1 .004-6.162l5.51-5.009a4.167 4.167 0 0 1 5.32-.236l81.228 61.62c2.725 2.067 6.639.124 6.639-3.297v.24a6.25 6.25 0 0 1-3.539 5.63Z"
          />
        </g>
        <g filter="url(#sideShadowFilterId)">
          <path
            fill="#1F9CF0"
            d="M75.858 99.126a6.232 6.232 0 0 1-7.108-1.21c2.306 2.307 6.25.674 6.25-2.588V4.672c0-3.262-3.944-4.895-6.25-2.589a6.232 6.232 0 0 1 7.108-1.21l20.6 9.908A6.25 6.25 0 0 1 100 16.413v67.174a6.25 6.25 0 0 1-3.541 5.633l-20.601 9.906Z"
          />
        </g>
        <path
          fill="url(#overlayGradientId)"
          fill-rule="evenodd"
          d="M70.851 99.317a6.224 6.224 0 0 0 4.96-.19L96.4 89.22a6.25 6.25 0 0 0 3.54-5.633V16.413a6.25 6.25 0 0 0-3.54-5.632L75.812.874a6.226 6.226 0 0 0-7.104 1.21L29.294 38.04 12.126 25.01a4.162 4.162 0 0 0-5.317.236l-5.507 5.009a4.168 4.168 0 0 0-.004 6.162L16.186 50 1.298 63.583a4.168 4.168 0 0 0 .004 6.162l5.507 5.009a4.162 4.162 0 0 0 5.317.236L29.294 61.96l39.414 35.958a6.218 6.218 0 0 0 2.143 1.4ZM74.954 27.3 45.048 50l29.906 22.701V27.3Z"
          clip-rule="evenodd"
          opacity=".25"
          style="mix-blend-mode:overlay"
        />
      </g>
      <defs>
        <filter
          id="topShadowFilterId"
          width="116.727"
          height="92.246"
          x="-8.394"
          y="15.829"
          color-interpolation-filters="sRGB"
          filterUnits="userSpaceOnUse"
>
          <feFlood flood-opacity="0" result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" />
          <feOffset />
          <feGaussianBlur stdDeviation="4.167" />
          <feColorMatrix values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.25 0" />
          <feBlend in2="BackgroundImageFix" mode="overlay" result="effect1_dropShadow" />
          <feBlend in="SourceGraphic" in2="effect1_dropShadow" result="shape" />
        </filter>
        <filter
          id="sideShadowFilterId"
          width="47.917"
          height="116.151"
          x="60.417"
          y="-8.076"
          color-interpolation-filters="sRGB"
          filterUnits="userSpaceOnUse"
>
          <feFlood flood-opacity="0" result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" />
          <feOffset />
          <feGaussianBlur stdDeviation="4.167" />
          <feColorMatrix values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.25 0" />
          <feBlend in2="BackgroundImageFix" mode="overlay" result="effect1_dropShadow" />
          <feBlend in="SourceGraphic" in2="effect1_dropShadow" result="shape" />
        </filter>
        <linearGradient
          id="overlayGradientId"
          x1="49.939"
          x2="49.939"
          y1=".258"
          y2="99.742"
          gradientUnits="userSpaceOnUse"
>
          <stop stop-color="#fff" />
          <stop offset="1" stop-color="#fff" stop-opacity="0" />
        </linearGradient>
      </defs>
    </svg>"##),
    ("icons/editors/vscode-insiders.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  fill="none" viewBox="0 0 100 100">
      <mask id="maskId" width="100" height="100" x="0" y="0" maskUnits="userSpaceOnUse">
        <path
          fill="#fff"
          fill-rule="evenodd"
          d="M70.912 99.317a6.223 6.223 0 0 0 4.96-.19l20.589-9.907A6.25 6.25 0 0 0 100 83.587V16.413a6.25 6.25 0 0 0-3.54-5.632L75.874.874a6.226 6.226 0 0 0-7.104 1.21L29.355 38.04 12.187 25.01a4.162 4.162 0 0 0-5.318.236l-5.506 5.009a4.168 4.168 0 0 0-.004 6.162L16.247 50 1.36 63.583a4.168 4.168 0 0 0 .004 6.162l5.506 5.01a4.162 4.162 0 0 0 5.318.236l17.168-13.032L68.77 97.917a6.217 6.217 0 0 0 2.143 1.4ZM75.015 27.3 45.11 50l29.906 22.701V27.3Z"
          clip-rule="evenodd"
        />
      </mask>
      <g mask="url(#maskId)">
        <path
          fill="#009a7c"
          d="M96.461 10.796 75.857.876a6.23 6.23 0 0 0-7.107 1.207l-67.451 61.5a4.167 4.167 0 0 0 .004 6.162l5.51 5.009a4.167 4.167 0 0 0 5.32.236l81.228-61.62c2.725-2.067 6.639-.124 6.639 3.297v-.24a6.25 6.25 0 0 0-3.539-5.63Z"
        />
        <g filter="url(#topShadowFilterId)">
          <path
            fill="#00b294"
            d="m96.461 89.204-20.604 9.92a6.229 6.229 0 0 1-7.107-1.207l-67.451-61.5a4.167 4.167 0 0 1 .004-6.162l5.51-5.009a4.167 4.167 0 0 1 5.32-.236l81.228 61.62c2.725 2.067 6.639.124 6.639-3.297v.24a6.25 6.25 0 0 1-3.539 5.63Z"
          />
        </g>
        <g filter="url(#sideShadowFilterId)">
          <path
            fill="#24bfa5"
            d="M75.858 99.126a6.232 6.232 0 0 1-7.108-1.21c2.306 2.307 6.25.674 6.25-2.588V4.672c0-3.262-3.944-4.895-6.25-2.589a6.232 6.232 0 0 1 7.108-1.21l20.6 9.908A6.25 6.25 0 0 1 100 16.413v67.174a6.25 6.25 0 0 1-3.541 5.633l-20.601 9.906Z"
          />
        </g>
        <path
          fill="url(#overlayGradientId)"
          fill-rule="evenodd"
          d="M70.851 99.317a6.224 6.224 0 0 0 4.96-.19L96.4 89.22a6.25 6.25 0 0 0 3.54-5.633V16.413a6.25 6.25 0 0 0-3.54-5.632L75.812.874a6.226 6.226 0 0 0-7.104 1.21L29.294 38.04 12.126 25.01a4.162 4.162 0 0 0-5.317.236l-5.507 5.009a4.168 4.168 0 0 0-.004 6.162L16.186 50 1.298 63.583a4.168 4.168 0 0 0 .004 6.162l5.507 5.009a4.162 4.162 0 0 0 5.317.236L29.294 61.96l39.414 35.958a6.218 6.218 0 0 0 2.143 1.4ZM74.954 27.3 45.048 50l29.906 22.701V27.3Z"
          clip-rule="evenodd"
          opacity=".25"
          style="mix-blend-mode:overlay"
        />
      </g>
      <defs>
        <filter
          id="topShadowFilterId"
          width="116.727"
          height="92.246"
          x="-8.394"
          y="15.829"
          color-interpolation-filters="sRGB"
          filterUnits="userSpaceOnUse"
>
          <feFlood flood-opacity="0" result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" />
          <feOffset />
          <feGaussianBlur stdDeviation="4.167" />
          <feColorMatrix values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.25 0" />
          <feBlend in2="BackgroundImageFix" mode="overlay" result="effect1_dropShadow" />
          <feBlend in="SourceGraphic" in2="effect1_dropShadow" result="shape" />
        </filter>
        <filter
          id="sideShadowFilterId"
          width="47.917"
          height="116.151"
          x="60.417"
          y="-8.076"
          color-interpolation-filters="sRGB"
          filterUnits="userSpaceOnUse"
>
          <feFlood flood-opacity="0" result="BackgroundImageFix" />
          <feColorMatrix in="SourceAlpha" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 127 0" />
          <feOffset />
          <feGaussianBlur stdDeviation="4.167" />
          <feColorMatrix values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.25 0" />
          <feBlend in2="BackgroundImageFix" mode="overlay" result="effect1_dropShadow" />
          <feBlend in="SourceGraphic" in2="effect1_dropShadow" result="shape" />
        </filter>
        <linearGradient
          id="overlayGradientId"
          x1="49.939"
          x2="49.939"
          y1=".258"
          y2="99.742"
          gradientUnits="userSpaceOnUse"
>
          <stop stop-color="#fff" />
          <stop offset="1" stop-color="#fff" stop-opacity="0" />
        </linearGradient>
      </defs>
    </svg>"##),
    ("icons/editors/vscodium.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 100 100">
      <defs>
        <linearGradient
          id="gradientId"
          x1="0"
          x2="100"
          y1="0"
          y2="100"
          gradientUnits="userSpaceOnUse"
>
          <stop offset="0" stop-color="#62A0EA" />
          <stop offset="1" stop-color="#1A5FB4" />
        </linearGradient>
      </defs>
      <path
        fill="url(#gradientId)"
        d="M48.26 2.274C45.406 4.105 44.583 7.898 46.422 10.742C56.531 26.397 58.917 38.205 57.882 48.553C53.698 68.369 44.603 72.389 36.655 72.389C28.895 72.389 30.973 59.618 36.806 55.88C40.288 53.706 44.748 52.293 48.171 52.293C51.563 52.293 54.313 49.552 54.313 46.17C54.313 42.787 51.563 40.046 48.171 40.046C44.173 40.046 40.251 40.886 36.59 42.316C37.338 38.787 37.614 34.973 36.647 30.919C35.179 24.763 30.953 18.883 23.615 13.183C22.33 12.183 20.7 11.734 19.083 11.934C17.466 12.134 15.995 12.966 14.994 14.248C12.912 16.918 13.394 20.766 16.072 22.843C22.05 27.486 24.024 30.923 24.699 33.752C25.374 36.581 24.831 39.616 23.475 43.786C21.742 49.406 19.73 54.423 18.848 59.234C18.414 61.602 18.377 64.179 18.265 66.238C13.96 62.042 12.275 56.502 12.275 48.407C12.274 45.025 9.524 42.283 6.133 42.284C2.744 42.287-0.002 45.027-0.003 48.407C-0.003 59.463 3.23 69.983 11.895 77.001C19.739 84.474 39.686 81.712 39.686 93.709C39.686 97.095 44.642 98.743 48.033 98.743C51.511 98.743 55.888 96.418 55.888 93.709C55.888 80.097 70.233 71.824 93.848 71.86C97.24 71.865 99.992 69.126 99.997 65.744C100.003 62.361 97.259 59.614 93.867 59.608C92.252 59.606 90.678 59.661 89.126 59.753C91.766 53.544 92.937 46.708 92.695 39.324C92.583 35.943 89.745 33.293 86.356 33.403C82.963 33.513 80.305 36.346 80.416 39.729C80.736 49.397 80.374 58.03 73.171 62.581C71.123 63.874 68.742 64.996 66.484 64.996C68.237 60.228 69.561 55.195 70.103 49.77C70.449 46.308 70.486 42.195 70.091 39C69.478 34.05 68.738 28.436 70.617 24.207C72.305 20.565 76.087 19.04 81.64 19.04C85.029 19.037 87.775 16.296 87.776 12.917C87.778 9.534 85.031 6.79 81.64 6.787C73.388 6.787 67.133 11.13 63.587 16.377C61.733 12.417 59.475 8.336 56.747 4.112C55.866 2.747 54.478 1.788 52.887 1.443C52.099 1.272 51.285 1.257 50.491 1.399C49.697 1.542 48.939 1.839 48.26 2.274z"
      />
    </svg>"##),
    ("icons/editors/zed.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  fill="none" viewBox="0 0 96 96">
      <g clipPath="url(#clipPathId)">
        <path
          fill="currentColor"
          fill-rule="evenodd"
          d="M9 6a3 3 0 0 0-3 3v66H0V9a9 9 0 0 1 9-9h80.379c4.009 0 6.016 4.847 3.182 7.682L43.055 57.187H57V51h6v7.688a4.5 4.5 0 0 1-4.5 4.5H37.055L26.743 73.5H73.5V36h6v37.5a6 6 0 0 1-6 6H20.743L10.243 90H87a3 3 0 0 0 3-3V21h6v66a9 9 0 0 1-9 9H6.621c-4.009 0-6.016-4.847-3.182-7.682L52.757 39H39v6h-6v-7.5a4.5 4.5 0 0 1 4.5-4.5h21.257l10.5-10.5H22.5V60h-6V22.5a6 6 0 0 1 6-6h52.757L85.757 6H9Z"
          clip-rule="evenodd"
        />
      </g>
      <defs>
        <clipPath id="clipPathId">
          <path fill="#fff" d="M0 0h96v96H0z" />
        </clipPath>
      </defs>
    </svg>"##),
    ("icons/editors/idea.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="-.391"
          x2="24.392"
          y1="7.671"
          y2="61.126"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".1" stop-color="#FC801D" />
          <stop offset=".59" stop-color="#FE2857" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="4.325"
          x2="62.921"
          y1="59.932"
          y2="1.336"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".21" stop-color="#FE2857" />
          <stop offset=".7" stop-color="#007EFF" />
        </linearGradient>
      </defs>
      <path
        fill="#FF8100"
        d="M16.45 6H4.191a4.125 4.125 0 0 0-4.124 4.19l.176 11.044a4.13 4.13 0 0 0 1.44 3.066l38.159 32.707c.747.64 1.7.993 2.684.993h11.35A4.125 4.125 0 0 0 58 53.875V42.872c0-1.19-.514-2.321-1.41-3.105L19.167 7.021A4.12 4.12 0 0 0 16.45 6"
      />
      <path
        fill="url(#gradientAId)"
        d="M14.988 6H4.125A4.125 4.125 0 0 0 0 10.125v12.566q0 .3.044.598l5.448 37.185A4.125 4.125 0 0 0 9.573 64h15.398a4.125 4.125 0 0 0 4.125-4.127L29.09 41.37c0-.426-.066-.849-.195-1.254l-9.98-31.245A4.13 4.13 0 0 0 14.988 6z"
      />
      <path
        fill="url(#gradientBId)"
        d="M59.876 0H25.748a4.13 4.13 0 0 0-3.8 2.52L6.151 39.943a4.1 4.1 0 0 0-.325 1.638l.15 18.329A4.125 4.125 0 0 0 10.101 64h17.666c.806 0 1.593-.236 2.266-.678l32.11-21.109A4.12 4.12 0 0 0 64 38.766V4.125A4.125 4.125 0 0 0 59.876 0"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM17 29.383h2.98v-9.775H17v-2.616h8.843v2.616h-2.98v9.775h2.98V32H17zM27.643 29.298h2.154a2.4 2.4 0 0 0 1.163-.279q.51-.279.788-.788.279-.51.279-1.163V16.992h2.926v10.28q0 1.35-.622 2.427a4.45 4.45 0 0 1-1.715 1.688q-1.092.612-2.454.611h-2.519z"
      />
    </svg>"##),
    ("icons/editors/aqua.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="59.932"
          x2="1.336"
          y1="59.676"
          y2="1.079"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".25" stop-color="#7256FF" />
          <stop offset=".73" stop-color="#007DFE" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="7.671"
          x2="61.125"
          y1="64.392"
          y2="39.609"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".3" stop-color="#00D886" />
          <stop offset=".54" stop-color="#7256FF" />
        </linearGradient>
      </defs>
      <path
        fill="url(#gradientAId)"
        d="M0 4.125v34.128c0 1.658.993 3.154 2.52 3.8L39.943 57.85c.518.219 1.075.33 1.638.324l18.329-.15A4.125 4.125 0 0 0 64 53.9V36.234c0-.806-.236-1.593-.678-2.267L42.213 1.86A4.13 4.13 0 0 0 38.766 0H4.125A4.125 4.125 0 0 0 0 4.125"
      />
      <path
        fill="url(#gradientBId)"
        d="M6 49.015v10.862a4.125 4.125 0 0 0 4.125 4.125h12.566q.3 0 .598-.044l37.185-5.448A4.125 4.125 0 0 0 64 54.429V39.03a4.125 4.125 0 0 0-4.127-4.125l-18.504.005c-.426 0-.849.066-1.254.195L8.871 45.085A4.13 4.13 0 0 0 6 49.015z"
      />
      <path
        fill="#00D886"
        d="M6 47.55v12.259a4.125 4.125 0 0 0 4.19 4.124L21 64c1.181-.019 2.531-.786 3.3-1.683l32.707-38.158c.64-.748.993-1.7.993-2.685V10.125A4.125 4.125 0 0 0 53.875 6H42.872c-1.19 0-2.321.514-3.105 1.409L7.021 44.834A4.12 4.12 0 0 0 6 47.55"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM20.746 31.243a7.3 7.3 0 0 1-2.755-2.787q-1.002-1.774-1.003-3.961c0-2.187.334-2.778 1.003-3.96a7.3 7.3 0 0 1 2.755-2.787q1.752-1.013 3.917-1.014c1.443 0 2.739.338 3.907 1.013a7.3 7.3 0 0 1 2.75 2.787q.996 1.774.996 3.961c0 2.187-.332 2.778-.997 3.96a7.3 7.3 0 0 1-2.749 2.788q-1.752 1.013-3.907 1.013c-1.436 0-2.75-.338-3.917-1.013m6.308-2.23q1.06-.67 1.661-1.854.6-1.185.6-2.664t-.6-2.664a4.6 4.6 0 0 0-1.661-1.854q-1.061-.67-2.39-.67t-2.396.67a4.6 4.6 0 0 0-1.672 1.854q-.605 1.185-.605 2.664t.605 2.664 1.672 1.854q1.067.67 2.396.67 1.33 0 2.39-.67m-2.16 6.034a2.67 2.67 0 0 1-1.126-1.077q-.391-.696-.391-1.64l-.01-1.94h2.743v1.811q0 .332.129.573.128.242.37.365.24.123.574.123h1.833v2.165h-2.412q-.975 0-1.71-.38M38.233 16.992h3.173l5.563 15.006h-2.947l-1.212-3.483h-5.842l-1.136 3.483H32.82zm3.805 9.261-2.025-5.723-.225-.954-.215.954-1.908 5.723z"
      />
    </svg>"##),
    ("icons/editors/clion.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="4.067"
          x2="62.664"
          y1="4.327"
          y2="62.923"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".29" stop-color="#009AE5" />
          <stop offset=".7" stop-color="#00D980" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="56.329"
          x2="2.874"
          y1="-.391"
          y2="24.393"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".3" stop-color="#FF2D90" />
          <stop offset=".54" stop-color="#009AE5" />
        </linearGradient>
      </defs>
      <path
        fill="url(#gradientAId)"
        d="M64 59.878V25.75a4.13 4.13 0 0 0-2.52-3.8L24.057 6.153a4.1 4.1 0 0 0-1.638-.325l-18.329.15A4.124 4.124 0 0 0 0 10.103v17.665c0 .806.236 1.593.678 2.267l21.109 32.109a4.12 4.12 0 0 0 3.447 1.859h34.641A4.125 4.125 0 0 0 64 59.878"
      />
      <path
        fill="url(#gradientBId)"
        d="M58 14.988V4.125A4.125 4.125 0 0 0 53.875 0H41.309q-.3 0-.598.044L3.527 5.492A4.125 4.125 0 0 0 0 9.573v15.398a4.125 4.125 0 0 0 4.126 4.125l18.505-.005c.425 0 .848-.066 1.253-.195l31.246-9.98A4.13 4.13 0 0 0 58 14.988"
      />
      <path
        fill="#FF2D90"
        d="M58 16.453V4.194A4.125 4.125 0 0 0 53.81.07L43.003.008c-1.18.019-2.535.781-3.304 1.678L6.993 39.844c-.64.748-.993 1.7-.993 2.684v11.35a4.125 4.125 0 0 0 4.125 4.125h11.003c1.19 0 2.321-.514 3.105-1.409L56.979 19.17A4.12 4.12 0 0 0 58 16.453"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM20.747 31.243a7.3 7.3 0 0 1-2.744-2.787q-.997-1.774-.997-3.961c0-2.187.332-2.778.997-3.96a7.3 7.3 0 0 1 2.744-2.787q1.747-1.013 3.901-1.014 1.823 0 3.345.675a6.86 6.86 0 0 1 2.535 1.892 6.44 6.44 0 0 1 1.355 2.793h-3.065a4.05 4.05 0 0 0-.895-1.431 4 4 0 0 0-1.431-.95 4.9 4.9 0 0 0-1.822-.332q-1.33 0-2.402.665a4.6 4.6 0 0 0-1.677 1.827q-.606 1.164-.606 2.62 0 1.459.606 2.621a4.6 4.6 0 0 0 1.677 1.828q1.072.664 2.402.664a4.9 4.9 0 0 0 1.822-.332 4.02 4.02 0 0 0 2.326-2.38h3.065a6.44 6.44 0 0 1-1.355 2.792 6.86 6.86 0 0 1-2.535 1.892q-1.522.675-3.345.675-2.154 0-3.901-1.013zM37.33 16.992v12.37h7.117v2.636H34.414V16.992z"
      />
    </svg>"##),
    ("icons/editors/datagrip.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="59.676"
          x2="1.08"
          y1="4.067"
          y2="62.663"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".28" stop-color="#7256FF" />
          <stop offset=".66" stop-color="#00D980" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="64.391"
          x2="39.607"
          y1="56.329"
          y2="2.874"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".3" stop-color="#FF43F2" />
          <stop offset=".54" stop-color="#7256FF" />
        </linearGradient>
      </defs>
      <path
        fill="url(#gradientAId)"
        d="M4.125 64h34.128a4.13 4.13 0 0 0 3.8-2.52L57.85 24.057c.219-.518.33-1.076.324-1.638l-.15-18.329A4.125 4.125 0 0 0 53.9 0H36.234c-.806 0-1.593.236-2.267.678L1.86 21.787A4.13 4.13 0 0 0 0 25.234v34.641A4.125 4.125 0 0 0 4.125 64"
      />
      <path
        fill="url(#gradientBId)"
        d="M49.013 58h10.862A4.125 4.125 0 0 0 64 53.875V41.309q0-.3-.044-.598L58.508 3.527A4.125 4.125 0 0 0 54.427 0H39.029a4.125 4.125 0 0 0-4.125 4.126l.005 18.505c0 .425.066.848.195 1.253l9.979 31.246a4.13 4.13 0 0 0 3.93 2.87"
      />
      <path
        fill="#FF43F2"
        d="M47.55 58h12.259a4.125 4.125 0 0 0 4.124-4.19L64 43c-.018-1.181-.785-2.531-1.682-3.3L24.159 6.993A4.13 4.13 0 0 0 21.474 6H10.125A4.125 4.125 0 0 0 6 10.125v11.003c0 1.19.514 2.321 1.409 3.105l37.425 32.746A4.12 4.12 0 0 0 47.55 58"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM17.012 16.992h5.713q2.133 0 3.821.965a6.9 6.9 0 0 1 2.642 2.674q.954 1.71.954 3.864t-.954 3.865a6.9 6.9 0 0 1-2.642 2.674q-1.688.964-3.821.964h-5.713zm8.028 11.876q1.008-.6 1.554-1.72.547-1.12.547-2.653 0-1.534-.547-2.653-.546-1.12-1.554-1.721-1.008-.6-2.348-.6h-2.755v9.947h2.755q1.34 0 2.348-.6M35.426 31.243a7.3 7.3 0 0 1-2.744-2.787q-.997-1.774-.997-3.961c0-2.187.333-2.778.997-3.96s1.58-2.112 2.744-2.787q1.747-1.013 3.902-1.014 1.746 0 3.221.622a6.9 6.9 0 0 1 2.487 1.747 6.36 6.36 0 0 1 1.42 2.594h-3.13a3.9 3.9 0 0 0-.927-1.228q-.585-.52-1.367-.803c-.782-.283-1.082-.284-1.683-.284q-1.329 0-2.401.664a4.6 4.6 0 0 0-1.678 1.828q-.605 1.163-.605 2.62c0 1.457.202 1.846.605 2.621a4.6 4.6 0 0 0 1.678 1.828q1.072.664 2.401.664 1.233 0 2.235-.461t1.592-1.276a3.3 3.3 0 0 0 .633-1.833l.01.31h-3.526v-2.304h6.356v1.18q0 1.982-.96 3.585a6.9 6.9 0 0 1-2.625 2.525q-1.668.921-3.736.921c-1.38 0-2.737-.337-3.902-1.013z"
      />
    </svg>"##),
    ("icons/editors/dataspell.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="4.067"
          x2="62.664"
          y1="4.327"
          y2="62.923"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".28" stop-color="#007DFE" />
          <stop offset=".73" stop-color="#00D980" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="56.329"
          x2="2.875"
          y1="-.391"
          y2="24.392"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".32" stop-color="#F0EB18" />
          <stop offset=".55" stop-color="#007DFE" />
        </linearGradient>
      </defs>
      <path
        fill="url(#gradientAId)"
        d="M64 59.878V25.75a4.13 4.13 0 0 0-2.52-3.8L24.057 6.153a4.1 4.1 0 0 0-1.638-.325l-18.329.15A4.124 4.124 0 0 0 0 10.103v17.665c0 .806.236 1.593.678 2.267l21.109 32.109a4.12 4.12 0 0 0 3.447 1.859h34.641A4.125 4.125 0 0 0 64 59.878"
      />
      <path
        fill="url(#gradientBId)"
        d="M58 14.988V4.125A4.125 4.125 0 0 0 53.875 0H41.309q-.3 0-.598.044L3.527 5.492A4.125 4.125 0 0 0 0 9.573v15.398a4.125 4.125 0 0 0 4.126 4.125l18.505-.005c.425 0 .848-.066 1.253-.195l31.246-9.98A4.13 4.13 0 0 0 58 14.988"
      />
      <path
        fill="#F0EB18"
        d="M58 16.45V4.191A4.125 4.125 0 0 0 53.81.067L43 0c-1.181.019-2.531.786-3.3 1.683L6.993 39.84c-.64.748-.993 1.7-.993 2.685v11.349A4.125 4.125 0 0 0 10.125 58h11.003c1.19 0 2.321-.514 3.105-1.409l32.746-37.425A4.12 4.12 0 0 0 58 16.45"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM17.012 16.992h5.713q2.133 0 3.821.965a6.9 6.9 0 0 1 2.642 2.674q.954 1.71.954 3.864t-.954 3.865a6.9 6.9 0 0 1-2.642 2.674q-1.688.964-3.821.964h-5.713zm8.028 11.876q1.008-.6 1.554-1.72.547-1.12.547-2.653 0-1.534-.547-2.653-.546-1.12-1.554-1.721-1.008-.6-2.348-.6h-2.755v9.947h2.755q1.34 0 2.348-.6M34.713 31.664q-1.26-.59-1.973-1.65t-.734-2.444h2.937q0 .654.343 1.147t.954.772q.61.278 1.404.278c.53 0 .952-.083 1.334-.251q.574-.252.89-.702t.316-1.029q0-.718-.434-1.19-.435-.471-1.195-.654l-2.648-.59q-1.071-.234-1.865-.809a4 4 0 0 1-1.232-1.42q-.44-.846-.44-1.908 0-1.286.665-2.31.664-1.023 1.854-1.597 1.19-.573 2.701-.573c1.511 0 1.933.186 2.733.558s1.424.888 1.871 1.549q.67.99.68 2.277h-2.926q0-.546-.289-.98c-.289-.435-.466-.515-.82-.676a2.9 2.9 0 0 0-1.217-.24q-.686 0-1.205.23a1.9 1.9 0 0 0-.81.643q-.29.412-.29.96 0 .621.403 1.028.402.407 1.098.579l2.552.557q1.103.226 1.967.852a4.45 4.45 0 0 1 1.345 1.544q.481.917.482 1.999 0 1.329-.702 2.385-.702 1.054-1.967 1.656-1.265.6-2.894.6t-2.889-.59"
      />
    </svg>"##),
    ("icons/editors/goland.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="64.391"
          x2="39.607"
          y1="56.329"
          y2="2.874"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".24" stop-color="#00D886" />
          <stop offset=".51" stop-color="#007DFE" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="59.676"
          x2="1.08"
          y1="4.067"
          y2="62.663"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".27" stop-color="#007DFE" />
          <stop offset=".7" stop-color="#D249FC" />
        </linearGradient>
      </defs>
      <path
        fill="#00D886"
        d="M47.55 58h12.259a4.125 4.125 0 0 0 4.124-4.19l-.176-11.044a4.13 4.13 0 0 0-1.44-3.066L24.159 6.993A4.13 4.13 0 0 0 21.474 6H10.125A4.125 4.125 0 0 0 6 10.125v11.003c0 1.19.514 2.321 1.409 3.105l37.425 32.746A4.12 4.12 0 0 0 47.55 58"
      />
      <path
        fill="url(#gradientAId)"
        d="M49.013 58h10.862A4.125 4.125 0 0 0 64 53.875V41.309q0-.3-.044-.598L58.508 3.527A4.124 4.124 0 0 0 54.427 0H39.029a4.125 4.125 0 0 0-4.125 4.126l.005 18.505c0 .425.066.848.195 1.253l9.979 31.246a4.13 4.13 0 0 0 3.93 2.87"
      />
      <path
        fill="url(#gradientBId)"
        d="M4.125 64h34.128a4.13 4.13 0 0 0 3.8-2.52L57.85 24.057c.219-.518.33-1.076.324-1.638l-.15-18.329A4.124 4.124 0 0 0 53.9 0H36.234c-.805 0-1.593.236-2.266.678L1.86 21.787A4.13 4.13 0 0 0 0 25.234v34.641A4.125 4.125 0 0 0 4.125 64"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M19.748 31.243a7.3 7.3 0 0 1-2.743-2.787q-.997-1.774-.997-3.961c0-2.187.332-2.778.997-3.96s1.58-2.112 2.743-2.787q1.747-1.013 3.902-1.014 1.747 0 3.222.622a6.9 6.9 0 0 1 2.486 1.747 6.4 6.4 0 0 1 1.42 2.594h-3.13a3.9 3.9 0 0 0-.926-1.228q-.584-.52-1.367-.803c-.783-.283-1.083-.284-1.683-.284q-1.33 0-2.402.664a4.6 4.6 0 0 0-1.677 1.828q-.606 1.163-.606 2.62c0 1.457.202 1.846.606 2.621a4.6 4.6 0 0 0 1.677 1.828q1.072.664 2.402.664 1.232 0 2.235-.461t1.591-1.276a3.3 3.3 0 0 0 .633-1.833l.01.31h-3.526v-2.304h6.357v1.18q0 1.982-.96 3.585a6.9 6.9 0 0 1-2.626 2.525q-1.666.921-3.736.921c-2.07 0-2.737-.337-3.902-1.013zM36.271 31.243a7.3 7.3 0 0 1-2.755-2.787q-1.002-1.774-1.002-3.961c0-2.187.333-2.778 1.002-3.96a7.3 7.3 0 0 1 2.755-2.787q1.752-1.013 3.918-1.014c1.443 0 2.738.338 3.907 1.013a7.3 7.3 0 0 1 2.749 2.787q.996 1.774.997 3.961c0 2.187-.333 2.778-.997 3.96s-1.581 2.113-2.75 2.788q-1.752 1.013-3.906 1.013c-1.437 0-2.75-.338-3.918-1.013m6.308-2.23q1.062-.67 1.662-1.854t.6-2.664-.6-2.664-1.662-1.854-2.39-.67-2.395.67a4.6 4.6 0 0 0-1.672 1.854q-.606 1.185-.606 2.664t.606 2.664 1.672 1.854 2.395.67q1.33 0 2.39-.67M33 44H17v3h16z"
      />
    </svg>"##),
    ("icons/editors/phpstorm.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="56.329"
          x2="2.874"
          y1="-.391"
          y2="24.392"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".16" stop-color="#D249FC" />
          <stop offset=".55" stop-color="#FF2D90" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="4.067"
          x2="62.664"
          y1="4.326"
          y2="62.923"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".3" stop-color="#FF2D90" />
          <stop offset=".7" stop-color="#7256FF" />
        </linearGradient>
      </defs>
      <path
        fill="#D249FC"
        d="M58 16.446V4.187A4.125 4.125 0 0 0 53.81.063L42.765.239A4.13 4.13 0 0 0 39.7 1.68L6.993 39.837c-.64.748-.993 1.7-.993 2.685v11.35a4.125 4.125 0 0 0 4.125 4.124h11.003c1.19 0 2.321-.514 3.105-1.409l32.746-37.425A4.12 4.12 0 0 0 58 16.446"
      />
      <path
        fill="url(#gradientAId)"
        d="M58 14.988V4.125A4.125 4.125 0 0 0 53.875 0H41.309q-.3 0-.598.044L3.527 5.492A4.125 4.125 0 0 0 0 9.573v15.398a4.125 4.125 0 0 0 4.126 4.125l18.505-.005c.425 0 .848-.066 1.253-.195l31.246-9.98A4.13 4.13 0 0 0 58 14.988"
      />
      <path
        fill="url(#gradientBId)"
        d="M64 59.878V25.75a4.13 4.13 0 0 0-2.52-3.8L24.057 6.153a4.1 4.1 0 0 0-1.638-.325l-18.329.15A4.124 4.124 0 0 0 0 10.103v17.665c0 .806.236 1.593.678 2.267l21.109 32.109a4.12 4.12 0 0 0 3.447 1.859h34.641A4.125 4.125 0 0 0 64 59.878"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM16.993 16.992h6.442q1.586 0 2.78.579 1.196.579 1.845 1.63.648 1.05.648 2.432c0 .922-.22 1.759-.659 2.466q-.66 1.062-1.87 1.646-1.212.584-2.83.584h-3.43V32h-2.927zm7.54 6.63q.553-.273.852-.782.3-.51.3-1.195c0-.685-.1-.842-.3-1.174q-.3-.5-.852-.772-.552-.273-1.291-.273h-3.323v4.47h3.323q.74 0 1.291-.273M32.572 31.664q-1.26-.59-1.972-1.65t-.735-2.444h2.937q0 .654.343 1.147t.954.772q.611.278 1.404.278c.53 0 .952-.083 1.335-.251q.572-.252.89-.702c.317-.45.315-.643.315-1.029q0-.718-.434-1.19-.435-.471-1.195-.654l-2.647-.59q-1.073-.234-1.866-.809a4 4 0 0 1-1.232-1.42q-.44-.846-.44-1.908 0-1.286.665-2.31.665-1.023 1.854-1.597 1.19-.573 2.702-.573c1.512 0 1.933.186 2.733.558q1.2.558 1.87 1.549t.681 2.277h-2.926q0-.546-.29-.98a1.9 1.9 0 0 0-.82-.676 2.9 2.9 0 0 0-1.216-.24q-.687 0-1.206.23a1.9 1.9 0 0 0-.81.643q-.29.412-.289.96 0 .621.402 1.028t1.099.579l2.551.557q1.104.226 1.967.852a4.45 4.45 0 0 1 1.345 1.544q.482.917.482 1.999 0 1.329-.701 2.385-.703 1.054-1.967 1.656-1.264.6-2.895.6-1.63 0-2.888-.59"
      />
    </svg>"##),
    ("icons/editors/pycharm.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="7.671"
          x2="61.126"
          y1="64.393"
          y2="39.609"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".1" stop-color="#00D886" />
          <stop offset=".59" stop-color="#F0EB18" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="59.933"
          x2="1.337"
          y1="59.676"
          y2="1.08"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".3" stop-color="#F0EB18" />
          <stop offset=".7" stop-color="#00C4F4" />
        </linearGradient>
      </defs>
      <path
        fill="#00D886"
        d="M6 47.55v12.259a4.125 4.125 0 0 0 4.19 4.124l11.044-.176a4.13 4.13 0 0 0 3.066-1.44l32.707-38.158c.64-.748.993-1.7.993-2.685V10.125A4.125 4.125 0 0 0 53.875 6H42.872c-1.19 0-2.321.514-3.105 1.409L7.021 44.834A4.12 4.12 0 0 0 6 47.55"
      />
      <path
        fill="url(#gradientAId)"
        d="M6 49.015v10.862a4.125 4.125 0 0 0 4.125 4.125h12.566q.3 0 .598-.044l37.185-5.448A4.125 4.125 0 0 0 64 54.429V39.03a4.125 4.125 0 0 0-4.127-4.125l-18.504.005c-.426 0-.849.066-1.254.195L8.871 45.085A4.13 4.13 0 0 0 6 49.015z"
      />
      <path
        fill="url(#gradientBId)"
        d="M0 4.125v34.128c0 1.658.993 3.154 2.52 3.8L39.943 57.85c.518.219 1.075.33 1.638.324l18.329-.15A4.125 4.125 0 0 0 64 53.9V36.234c0-.806-.236-1.593-.678-2.267L42.213 1.86A4.13 4.13 0 0 0 38.766 0H4.125A4.125 4.125 0 0 0 0 4.125"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM16.993 16.992h6.442q1.586 0 2.78.579 1.196.579 1.845 1.63.648 1.05.648 2.432c0 .922-.22 1.759-.659 2.466q-.66 1.062-1.87 1.646-1.212.584-2.83.584h-3.43V32h-2.927zm7.54 6.63q.553-.273.852-.782.3-.51.3-1.195c0-.685-.1-.842-.3-1.174q-.3-.5-.852-.772-.552-.273-1.291-.273h-3.324v4.47h3.324q.74 0 1.291-.273M33.713 31.243a7.3 7.3 0 0 1-2.744-2.787q-.996-1.774-.996-3.961c0-2.187.332-2.778.996-3.96a7.3 7.3 0 0 1 2.744-2.787q1.748-1.013 3.902-1.014 1.823 0 3.344.675a6.86 6.86 0 0 1 2.535 1.892 6.44 6.44 0 0 1 1.356 2.793h-3.066a4.05 4.05 0 0 0-.895-1.431 4 4 0 0 0-1.43-.95 4.9 4.9 0 0 0-1.823-.332q-1.33 0-2.402.665a4.6 4.6 0 0 0-1.677 1.827q-.606 1.164-.606 2.62 0 1.459.606 2.621a4.6 4.6 0 0 0 1.677 1.828q1.072.664 2.402.664.986 0 1.822-.332a4.02 4.02 0 0 0 2.326-2.38h3.066a6.44 6.44 0 0 1-1.356 2.792 6.86 6.86 0 0 1-2.535 1.892q-1.522.675-3.344.675-2.154 0-3.902-1.013z"
      />
    </svg>"##),
    ("icons/editors/rider.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="64.391"
          x2="39.607"
          y1="56.329"
          y2="2.874"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".21" stop-color="#007DFE" />
          <stop offset=".55" stop-color="#FFB700" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="59.676"
          x2="1.08"
          y1="4.067"
          y2="62.663"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".23" stop-color="#FFB700" />
          <stop offset=".73" stop-color="#FF0A67" />
        </linearGradient>
      </defs>
      <path
        fill="#007DFE"
        d="M47.55 58h12.258a4.125 4.125 0 0 0 4.124-4.19l-.176-11.044a4.12 4.12 0 0 0-1.44-3.066L24.158 6.993A4.13 4.13 0 0 0 21.474 6H10.125A4.125 4.125 0 0 0 6 10.125v11.003c0 1.19.514 2.321 1.409 3.105l37.425 32.746A4.12 4.12 0 0 0 47.55 58"
      />
      <path
        fill="url(#gradientAId)"
        d="M49.013 58h10.862A4.125 4.125 0 0 0 64 53.875V41.309q0-.3-.044-.598L58.508 3.527A4.125 4.125 0 0 0 54.427 0H39.029a4.125 4.125 0 0 0-4.125 4.126l.005 18.505c0 .425.066.848.195 1.253l9.979 31.246a4.13 4.13 0 0 0 3.93 2.87"
      />
      <path
        fill="url(#gradientBId)"
        d="M4.125 64h34.128a4.13 4.13 0 0 0 3.8-2.52L57.85 24.057c.219-.518.33-1.076.324-1.638l-.15-18.329A4.124 4.124 0 0 0 53.9 0H36.234c-.805 0-1.593.236-2.266.678L1.86 21.787A4.13 4.13 0 0 0 0 25.234v34.641A4.125 4.125 0 0 0 4.125 64"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM16.992 16.992h6.442q1.576 0 2.776.579t1.85 1.63.648 2.432c0 .922-.22 1.75-.66 2.46q-.66 1.068-1.875 1.651-1.217.584-2.825.584h-3.43v5.67h-2.926zm7.54 6.63q.553-.274.853-.783.3-.51.3-1.184c0-.45-.1-.852-.3-1.185q-.3-.498-.852-.772-.552-.273-1.292-.273h-3.323v4.47h3.323q.74 0 1.292-.273m-2.63 1.763h3.194L29.03 32h-3.355zM31.613 16.992h5.713q2.133 0 3.822.965a6.9 6.9 0 0 1 2.641 2.674q.954 1.71.954 3.864t-.953 3.865a6.9 6.9 0 0 1-2.642 2.674q-1.688.964-3.822.964h-5.713zm8.028 11.876q1.008-.6 1.555-1.72t.547-2.653q0-1.534-.547-2.653-.547-1.12-1.555-1.721-1.008-.6-2.347-.6h-2.755v9.947h2.755q1.34 0 2.347-.6"
      />
    </svg>"##),
    ("icons/editors/rubymine.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="4.325"
          x2="62.921"
          y1="59.932"
          y2="1.337"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".29" stop-color="#FF2358" />
          <stop offset=".75" stop-color="#7256FF" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="-.391"
          x2="24.393"
          y1="7.671"
          y2="61.125"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".29" stop-color="#FF8100" />
          <stop offset=".56" stop-color="#FF2358" />
        </linearGradient>
      </defs>
      <path
        fill="url(#gradientAId)"
        d="M59.875 0H25.748a4.13 4.13 0 0 0-3.8 2.52L6.151 39.942a4.1 4.1 0 0 0-.325 1.639l.15 18.328A4.125 4.125 0 0 0 10.101 64h17.666c.805 0 1.593-.235 2.266-.678l32.109-21.108A4.12 4.12 0 0 0 64 38.766V4.125A4.125 4.125 0 0 0 59.875 0"
      />
      <path
        fill="url(#gradientBId)"
        d="M14.987 6H4.126A4.125 4.125 0 0 0 0 10.125v12.566q0 .3.044.598l5.448 37.184A4.125 4.125 0 0 0 9.572 64H24.97a4.125 4.125 0 0 0 4.125-4.126l-.004-18.504c0-.426-.067-.85-.196-1.254L18.917 8.87A4.13 4.13 0 0 0 14.987 6z"
      />
      <path
        fill="#FF8100"
        d="M16.45 6H4.19a4.125 4.125 0 0 0-4.124 4.19L0 21c.019 1.181.786 2.531 1.683 3.3l38.158 32.706c.748.641 1.7.993 2.684.993h11.35A4.125 4.125 0 0 0 58 53.875V42.872c0-1.189-.514-2.32-1.41-3.104L19.167 7.021A4.12 4.12 0 0 0 16.45 6"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM17.012 16.992h6.442q1.575 0 2.776.579t1.849 1.63.648 2.432c0 .922-.22 1.75-.66 2.46q-.658 1.068-1.875 1.651-1.216.584-2.824.584h-3.43v5.67h-2.926zm7.54 6.63q.552-.274.852-.783.3-.51.3-1.184c0-.45-.1-.852-.3-1.185q-.3-.498-.852-.772-.552-.273-1.292-.273h-3.323v4.47h3.323q.74 0 1.292-.273m-2.631 1.763h3.194L29.05 32h-3.355zM31.633 16.992h4.073l3.087 9.85.257 1.287.225-1.286 2.98-9.85h4.138v15.005h-2.894V21.29l.043-.782-3.494 11.49h-2.123l-3.451-11.415.043.707v10.708h-2.883V16.992z"
      />
    </svg>"##),
    ("icons/editors/rustrover.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="7.671"
          x2="61.125"
          y1="64.393"
          y2="39.609"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".08" stop-color="#00D886" />
          <stop offset=".46" stop-color="#FFAB00" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="59.932"
          x2="1.336"
          y1="59.676"
          y2="1.08"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".19" stop-color="#FFAB00" />
          <stop offset=".83" stop-color="#FF004C" />
        </linearGradient>
      </defs>
      <path
        fill="#00D886"
        d="M6 47.55v12.258a4.125 4.125 0 0 0 4.19 4.124l11.044-.176a4.12 4.12 0 0 0 3.066-1.44l32.707-38.158c.64-.747.993-1.7.993-2.684V10.125A4.125 4.125 0 0 0 53.875 6H42.872c-1.19 0-2.321.514-3.105 1.409L7.021 44.833A4.12 4.12 0 0 0 6 47.55"
      />
      <path
        fill="url(#gradientAId)"
        d="M6 49.015v10.862a4.125 4.125 0 0 0 4.125 4.125h12.566q.3 0 .598-.044l37.185-5.448A4.125 4.125 0 0 0 64 54.429V39.03a4.125 4.125 0 0 0-4.127-4.125l-18.504.005c-.426 0-.849.066-1.254.195L8.871 45.085A4.13 4.13 0 0 0 6 49.015z"
      />
      <path
        fill="url(#gradientBId)"
        d="M0 4.125v34.128c0 1.658.993 3.154 2.52 3.8L39.943 57.85c.518.219 1.075.33 1.638.324l18.329-.15A4.125 4.125 0 0 0 64 53.9V36.234c0-.806-.236-1.593-.678-2.267L42.213 1.86A4.13 4.13 0 0 0 38.766 0H4.125A4.125 4.125 0 0 0 0 4.125"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM16.992 16.992h6.442q1.576 0 2.776.579t1.85 1.63.648 2.432c0 .922-.22 1.75-.66 2.46q-.66 1.068-1.875 1.651-1.217.584-2.825.584h-3.43v5.67h-2.926zm7.54 6.63q.553-.274.853-.783.3-.51.3-1.184c0-.45-.1-.852-.3-1.185q-.3-.498-.852-.772-.552-.273-1.292-.273h-3.323v4.47h3.323q.74 0 1.292-.273m-2.63 1.763h3.194L29.03 32h-3.355zM31.613 16.992h6.442q1.575 0 2.776.579t1.85 1.63.648 2.432c0 .922-.22 1.75-.66 2.46q-.659 1.068-1.875 1.651-1.217.584-2.824.584h-3.43v5.67h-2.927zm7.54 6.63q.553-.274.853-.783.3-.51.3-1.184c0-.45-.1-.852-.3-1.185q-.3-.498-.852-.772-.552-.273-1.292-.273h-3.323v4.47h3.323q.74 0 1.292-.273m-2.63 1.763h3.194L43.651 32h-3.355z"
      />
    </svg>"##),
    ("icons/editors/webstorm.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"  viewBox="0 0 64 64" fill="none">
      <defs>
        <linearGradient
          id="gradientAId"
          x1="7.671"
          x2="61.126"
          y1="64.392"
          y2="39.609"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".22" stop-color="#F0EB18" />
          <stop offset=".59" stop-color="#00C4F4" />
        </linearGradient>
        <linearGradient
          id="gradientBId"
          x1="59.932"
          x2="1.337"
          y1="59.676"
          y2="1.079"
          gradientUnits="userSpaceOnUse"
>
          <stop offset=".19" stop-color="#00C4F4" />
          <stop offset=".83" stop-color="#007DFE" />
        </linearGradient>
      </defs>
      <path
        fill="#F0EB18"
        d="M6 47.55v12.258a4.125 4.125 0 0 0 4.19 4.124l11.044-.176a4.12 4.12 0 0 0 3.066-1.44l32.707-38.158c.64-.747.993-1.7.993-2.684V10.125A4.125 4.125 0 0 0 53.875 6H42.872c-1.19 0-2.321.514-3.105 1.409L7.021 44.833A4.12 4.12 0 0 0 6 47.55"
      />
      <path
        fill="url(#gradientAId)"
        d="M6 49.015v10.862a4.125 4.125 0 0 0 4.125 4.125h12.566q.3 0 .598-.044l37.185-5.448A4.125 4.125 0 0 0 64 54.429V39.03a4.125 4.125 0 0 0-4.127-4.125l-18.504.005c-.426 0-.849.066-1.254.195L8.871 45.085A4.13 4.13 0 0 0 6 49.015z"
      />
      <path
        fill="url(#gradientBId)"
        d="M0 4.125v34.128c0 1.658.993 3.154 2.52 3.8L39.943 57.85c.518.219 1.075.33 1.638.324l18.329-.15A4.125 4.125 0 0 0 64 53.9V36.234c0-.806-.236-1.593-.678-2.267L42.213 1.86A4.13 4.13 0 0 0 38.766 0H4.125A4.125 4.125 0 0 0 0 4.125"
      />
      <path fill="#000" d="M52 12H12v40h40z" />
      <path
        fill="#fff"
        d="M33 44H17v3h16zM19.051 16.992l2.423 10.955 2.583-10.955h2.958l2.701 10.955 2.348-10.955h2.97l-3.645 15.006h-3.334l-2.53-10.9-2.561 10.9H19.64l-3.623-15.006zM38.662 31.664q-1.26-.59-1.972-1.65t-.735-2.444h2.937q0 .654.343 1.147t.954.772q.61.278 1.404.278c.53 0 .952-.083 1.335-.251q.573-.252.889-.702t.316-1.029q0-.718-.434-1.19-.435-.471-1.195-.654l-2.648-.59q-1.071-.234-1.865-.809a4 4 0 0 1-1.232-1.42q-.44-.846-.44-1.908 0-1.286.665-2.31.664-1.023 1.854-1.597 1.19-.573 2.701-.573c1.008 0 1.934.186 2.734.558q1.2.558 1.87 1.549t.68 2.277h-2.925q0-.546-.29-.98c-.29-.435-.466-.515-.82-.676a2.9 2.9 0 0 0-1.217-.24q-.686 0-1.205.23a1.9 1.9 0 0 0-.81.643q-.29.412-.289.96 0 .621.402 1.028t1.099.579l2.55.557q1.105.226 1.968.852a4.45 4.45 0 0 1 1.345 1.544 4.2 4.2 0 0 1 .482 1.999q0 1.329-.702 2.385-.702 1.054-1.966 1.656-1.266.6-2.894.6-1.63 0-2.889-.59"
      />
    </svg>"##),
    ("icons/editors/finder.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none">
    <rect x="2" y="2" width="20" height="20" rx="4" fill="#36A9F5" />
    <path
      d="M13 2h5a4 4 0 0 1 4 4v12a4 4 0 0 1-4 4h-6c-1-4-1-7 0-10H9c0-4 2-8 4-10Z"
      fill="#D9F1FF"
    />
    <path
      d="M7 7v2m10-2v2M6 15c3 3 9 3 12 0"
      stroke="#163A59"
      stroke-width="1.5"
      stroke-linecap="round"
    />
  </svg>"##),
    ("icons/editors/file-explorer.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none">
    <path
      d="M2 5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2Z"
      fill="#D99A16"
    />
    <path d="M2 9h20v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2Z" fill="#FFCE45" />
    <path d="M8 14h8v7H8Z" fill="#58B8E8" />
    <path d="M10 16h4v2h-4Z" fill="#1879B9" />
  </svg>"##),
    ("icons/source-control/azure-devops.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 512 512" fill="none">
  <defs>
    <linearGradient id="a" x1="58.027" y1="356.668" x2="58.027" y2="100.668" gradientTransform="matrix(1 0 0 -1 0 514)" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#163697"/>
      <stop offset="0.276" stop-color="#2052cb"/>
      <stop offset="0.518" stop-color="#2764e7"/>
      <stop offset="0.879" stop-color="#367af2"/>
    </linearGradient>
    <linearGradient id="b" x1="58.027" y1="351.334" x2="58.027" y2="292.668" gradientTransform="matrix(1 0 0 -1 0 514)" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#102784"/>
      <stop offset="1" stop-color="#2052cb" stop-opacity="0"/>
    </linearGradient>
    <linearGradient id="c" x1="389.98" y1="57.935" x2="107.314" y2="193.935" gradientTransform="matrix(1 0 0 -1 0 514)" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#163697"/>
      <stop offset="0.301" stop-color="#2052cb"/>
      <stop offset="0.626" stop-color="#2764e7"/>
      <stop offset="0.926" stop-color="#367af2"/>
    </linearGradient>
    <linearGradient id="d" x1="391.996" y1="76.552" x2="341.385" y2="78.224" gradientTransform="matrix(1 0 0 -1 0 514)" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#102784"/>
      <stop offset="1" stop-color="#2052cb" stop-opacity="0"/>
    </linearGradient>
    <linearGradient id="e" x1="442.24" y1="54.769" x2="442.24" y2="419.38" gradientTransform="matrix(1 0 0 -1 0 514)" gradientUnits="userSpaceOnUse">
      <stop offset="0.043" stop-color="#2052cb"/>
      <stop offset="0.489" stop-color="#367af2"/>
      <stop offset="0.943" stop-color="#16bbda"/>
    </linearGradient>
    <linearGradient id="f" x1="435.596" y1="270.413" x2="425.732" y2="385.266" gradientTransform="matrix(1 0 0 -1 0 514)" gradientUnits="userSpaceOnUse">
      <stop offset="0.134" stop-color="#16bbda" stop-opacity="0"/>
      <stop offset="0.932" stop-color="#6be7a0" stop-opacity="0.8629"/>
    </linearGradient>
    <linearGradient id="g" x1="437.081" y1="204.362" x2="507.893" y2="119.081" gradientTransform="matrix(1 0 0 -1 0 514)" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#2c68e0" stop-opacity="0"/>
      <stop offset="0.898" stop-color="#66c0ff" stop-opacity="0.5"/>
    </linearGradient>
    <linearGradient id="h" x1="99.399" y1="279.26" x2="352.75" y2="498.676" gradientTransform="matrix(1 0 0 -1 0 514)" gradientUnits="userSpaceOnUse">
      <stop offset="0.005" stop-color="#367af2"/>
      <stop offset="0.507" stop-color="#0fafff"/>
      <stop offset="1" stop-color="#26cfe8"/>
    </linearGradient>
    <linearGradient id="i" x1="184.342" y1="356.795" x2="338.581" y2="480.642" gradientTransform="matrix(1 0 0 -1 0 514)" gradientUnits="userSpaceOnUse">
      <stop offset="0.443" stop-color="#26cfe8" stop-opacity="0"/>
      <stop offset="0.897" stop-color="#6be7a0" stop-opacity="0.8629"/>
    </linearGradient>
  </defs>
  <path d="M116.1 73.4c-5.8 5.8-13.7 13.8-13.7 22.6v216.4c0 8.9-3.3 17.3-9 23.4s-13.3 9.2-21.1 8.5l-42.7-3.1c-7.9-.5-15-3.9-20.3-9.3-5.7-5.7-9.3-13.7-9.3-22.6V202.7c0-8.9 3.6-16.9 9.4-22.6l2-1.9z" fill="url(#a)"/>
  <path d="M116.1 73.4c-5.8 5.8-13.7 13.8-13.7 22.6v216.4c0 8.9-3.3 17.3-9 23.4s-13.3 9.2-21.1 8.5l-42.7-3.1c-7.9-.5-15-3.9-20.3-9.3-5.7-5.7-9.3-13.7-9.3-22.6V202.7c0-8.9 3.6-16.9 9.4-22.6l2-1.9z" fill="url(#b)"/>
  <path d="m500.5 366-128 106.7c-5.5 4.6-12.7 7.4-20.5 7.4q-2.4 0-4.8-.3l-112.5-16.9v27.8c0 11.7-9.6 21.3-21.3 21.3-7.3 0-13.5-3.6-17.5-9.1l-36.2-51.4-5.4-7.6-36.6-51.9-7-10c-2.6-3.5-3.9-7.8-3.9-12.3 0-6 2.5-11.6 6.8-15.7 3.8-3.7 9.1-5.8 14.5-5.8.4 0 1 .1 1.5.1l105.2 7.5L384 366.5l93.7 6.7c.7.1 1.6.1 2.3.1 7.8 0 14.9-2.7 20.5-7.3" fill="url(#c)"/>
  <path d="m500.5 366-128 106.7c-5.5 4.6-12.7 7.4-20.5 7.4q-2.4 0-4.8-.3l-112.5-16.9v27.8c0 11.7-9.6 21.3-21.3 21.3-7.3 0-13.5-3.6-17.5-9.1l-36.2-51.4-5.4-7.6-36.6-51.9-7-10c-2.6-3.5-3.9-7.8-3.9-12.3 0-6 2.5-11.6 6.8-15.7 3.8-3.7 9.1-5.8 14.5-5.8.4 0 1 .1 1.5.1l105.2 7.5L384 366.5l93.7 6.7c.7.1 1.6.1 2.3.1 7.8 0 14.9-2.7 20.5-7.3" fill="url(#d)"/>
  <path d="M512 170.7v170.7c0 9.9-4.5 18.8-11.5 24.6l-128 106.7c7-5.9 11.5-14.7 11.5-24.6V175.3c0-16.7 13-30.7 29.8-31.9l63.9-4.6c.7-.1 1.6-.1 2.3-.1 7.8 0 14.9 2.8 20.5 7.4 7 5.8 11.5 14.6 11.5 24.6" fill="url(#e)"/>
  <path d="M512 170.7v170.7c0 9.9-4.5 18.8-11.5 24.6l-128 106.7c7-5.9 11.5-14.7 11.5-24.6V175.3c0-16.7 13-30.7 29.8-31.9l63.9-4.6c.7-.1 1.6-.1 2.3-.1 7.8 0 14.9 2.8 20.5 7.4 7 5.8 11.5 14.6 11.5 24.6" fill="url(#f)" fill-opacity="0.7"/>
  <path d="M512 170.7v170.7c0 9.9-4.5 18.8-11.5 24.6l-128 106.7c7-5.9 11.5-14.7 11.5-24.6V175.3c0-16.7 13-30.7 29.8-31.9l63.9-4.6c.7-.1 1.6-.1 2.3-.1 7.8 0 14.9 2.8 20.5 7.4 7 5.8 11.5 14.6 11.5 24.6" fill="url(#g)"/>
  <path d="M384 125.5c0 11.2-8.6 20.5-19.8 21.3-.5 0-1.1.1-1.6.1l-127.9 9.2-132.3 9.1-72.7 5.5c-6.9.4-13.2 3.1-18.2 7.4L116.1 73.4c4.7-4.7 10.9-8 17.8-9.1l100.8-15.1V21.3C234.7 9.6 244.3 0 256 0c5.8 0 11.1 2.3 14.9 6.1l33.4 32.6 16.9 16.5 40.3 39.3 16.1 15.8c4.1 3.9 6.4 9.4 6.4 15.2" fill="url(#h)"/>
  <path d="M384 125.5c0 11.2-8.6 20.5-19.8 21.3-.5 0-1.1.1-1.6.1l-127.9 9.2-132.3 9.1-72.7 5.5c-6.9.4-13.2 3.1-18.2 7.4L116.1 73.4c4.7-4.7 10.9-8 17.8-9.1l100.8-15.1V21.3C234.7 9.6 244.3 0 256 0c5.8 0 11.1 2.3 14.9 6.1l33.4 32.6 16.9 16.5 40.3 39.3 16.1 15.8c4.1 3.9 6.4 9.4 6.4 15.2" fill="url(#i)" fill-opacity="0.5"/>
</svg>"##),
    ("icons/source-control/bitbucket.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="8.4 14.39 2481.29 2231.21" fill="none">
  <defs>
    <linearGradient id="a" x1="945.1094" y1="1524.8389" x2="944.4923" y2="1524.1893" gradientTransform="matrix(1996.6343 0 0 -1480.3047 -1884485.625 2258195)" gradientUnits="userSpaceOnUse">
      <stop offset="0.18" stop-color="#0052CC"/>
      <stop offset="1" stop-color="#2684FF"/>
    </linearGradient>
  </defs>
  <path d="M88.92,14.4C45.02,13.83,8.97,48.96,8.41,92.86c-0.06,4.61,0.28,9.22,1.02,13.77l337.48,2048.72 c8.68,51.75,53.26,89.8,105.74,90.24h1619.03c39.38,0.5,73.19-27.9,79.49-66.78l337.49-2071.78c7.03-43.34-22.41-84.17-65.75-91.2 c-4.55-0.74-9.15-1.08-13.76-1.02L88.92,14.4z M1509.99,1495.09H993.24l-139.92-731h781.89L1509.99,1495.09z" fill="#2684FF"/>
  <path d="M2379.27,763.06h-745.5l-125.12,730.42H992.31l-609.67,723.67c19.32,16.71,43.96,26,69.5,26.21h1618.13 c39.35,0.51,73.14-27.88,79.44-66.72L2379.27,763.06z" fill="url(#a)"/>
</svg>"##),
    ("icons/source-control/cursor-origin.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 466.73 532.09" fill="none">
  <path d="M457.43,125.94L244.42,2.96c-6.84-3.95-15.28-3.95-22.12,0L9.3,125.94c-5.75,3.32-9.3,9.46-9.3,16.11v247.99c0,6.65,3.55,12.79,9.3,16.11l213.01,122.98c6.84,3.95,15.28,3.95,22.12,0l213.01-122.98c5.75-3.32,9.3-9.46,9.3-16.11v-247.99c0-6.65-3.55-12.79-9.3-16.11h-.01ZM444.05,151.99l-205.63,356.16c-1.39,2.4-5.06,1.42-5.06-1.36v-233.21c0-4.66-2.49-8.97-6.53-11.31L24.87,145.67c-2.4-1.39-1.42-5.06,1.36-5.06h411.26c5.84,0,9.49,6.33,6.57,11.39h-.01Z" fill="currentColor"/>
</svg>"##),
    ("icons/source-control/forgejo.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="-6 -6 212 212" fill="none">
  <path d="M58 168 v-98 a50 50 0 0 1 50-50 h20" fill="none" stroke="#ff6600" stroke-width="25"/>
  <path d="M58 168 v-30 a50 50 0 0 1 50-50 h20" fill="none" stroke="#d40000" stroke-width="25"/>
  <path d="M124 20 a18 18 0 1 0 36 0 a18 18 0 1 0 -36 0z" fill="none" stroke="#ff6600" stroke-width="15"/>
  <path d="M124 88 a18 18 0 1 0 36 0 a18 18 0 1 0 -36 0z" fill="none" stroke="#d40000" stroke-width="15"/>
  <path d="M40 180 a18 18 0 1 0 36 0 a18 18 0 1 0 -36 0z" fill="none" stroke="#d40000" stroke-width="15"/>
</svg>"##),
    ("icons/source-control/gitcafe.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 64 64" fill="none">
  <path d="M4.8 4.8h54.4v54.4H4.8z" fill="#a78bfa"/>
</svg>"##),
    ("icons/source-control/github.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 16 16" fill="none">
  <path d="M8 0C3.58 0 0 3.58 0 8C0 11.54 2.29 14.53 5.47 15.59C5.87 15.66 6.02 15.42 6.02 15.21C6.02 15.02 6.01 14.39 6.01 13.72C4 14.09 3.48 13.23 3.32 12.78C3.23 12.55 2.84 11.84 2.5 11.65C2.22 11.5 1.82 11.13 2.49 11.12C3.12 11.11 3.57 11.7 3.72 11.94C4.44 13.15 5.59 12.81 6.05 12.6C6.12 12.08 6.33 11.73 6.56 11.53C4.78 11.33 2.92 10.64 2.92 7.58C2.92 6.71 3.23 5.99 3.74 5.43C3.66 5.23 3.38 4.41 3.82 3.31C3.82 3.31 4.49 3.1 6.02 4.13C6.66 3.95 7.34 3.86 8.02 3.86C8.7 3.86 9.38 3.95 10.02 4.13C11.55 3.09 12.22 3.31 12.22 3.31C12.66 4.41 12.38 5.23 12.3 5.43C12.81 5.99 13.12 6.7 13.12 7.58C13.12 10.65 11.25 11.33 9.47 11.53C9.76 11.78 10.01 12.26 10.01 13.01C10.01 14.08 10 14.94 10 15.21C10 15.42 10.15 15.67 10.55 15.59C13.71 14.53 16 11.53 16 8C16 3.58 12.42 0 8 0Z" fill="currentColor" fill-rule="evenodd"/>
</svg>"##),
    ("icons/source-control/gitlab.svg", br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 32 32" fill="none">
  <path d="m31.46 12.78-.04-.12-4.35-11.35A1.14 1.14 0 0 0 25.94.6c-.24 0-.47.1-.66.24-.19.15-.33.36-.39.6l-2.94 9h-11.9l-2.94-9A1.14 1.14 0 0 0 6.07.58a1.15 1.15 0 0 0-1.14.72L.58 12.68l-.05.11a8.1 8.1 0 0 0 2.68 9.34l.02.01.04.03 6.63 4.97 3.28 2.48 2 1.52a1.35 1.35 0 0 0 1.62 0l2-1.52 3.28-2.48 6.67-5h.02a8.09 8.09 0 0 0 2.7-9.36Z" fill="#E24329"/>
  <path d="m31.46 12.78-.04-.12a14.75 14.75 0 0 0-5.86 2.64l-9.55 7.24 6.09 4.6 6.67-5h.02a8.09 8.09 0 0 0 2.67-9.36Z" fill="#FC6D26"/>
  <path d="m9.9 27.14 3.28 2.48 2 1.52a1.35 1.35 0 0 0 1.62 0l2-1.52 3.28-2.48-6.1-4.6-6.07 4.6Z" fill="#FCA326"/>
  <path d="M6.44 15.3a14.71 14.71 0 0 0-5.86-2.63l-.05.12a8.1 8.1 0 0 0 2.68 9.34l.02.01.04.03 6.63 4.97 6.1-4.6-9.56-7.24Z" fill="#FC6D26"/>
</svg>"##),
];

impl ProviderLogo {
    pub fn render(self, size: impl Into<Length>, color: Hsla, dark: bool) -> AnyElement {
        let size = size.into();
        match self {
            Self::Mono(path) => svg().path(path).size(size).flex_none().text_color(color).into_any_element(),
            Self::Image { light, dark: dark_path } => img(if dark { dark_path } else { light }).size(size).flex_none().into_any_element(),
        }
    }

    const fn fixed(path: &'static str) -> Self {
        Self::Image { light: path, dark: path }
    }
}

/// An Open In brand, using the app platform (darwin/win32/linux) for file managers.
#[allow(dead_code, reason = "Used by the Open In controls when their callers are migrated")]
pub fn editor_logo(editor_id: &str, platform: &str) -> Option<ProviderLogo> {
    Some(match editor_id {
        "cursor" => ProviderLogo::Image {
            light: "icons/editors/cursor-light.svg",
            dark: "icons/editors/cursor-dark.svg",
        },
        "antigravity" => provider_logo("antigravity")?,
        "trae" => ProviderLogo::Mono("icons/editors/trae.svg"),
        "zed" => ProviderLogo::Mono("icons/editors/zed.svg"),
        "file-manager" if platform == "darwin" => ProviderLogo::fixed("icons/editors/finder.svg"),
        "file-manager" if matches!(platform, "win32" | "windows") => ProviderLogo::fixed("icons/editors/file-explorer.svg"),
        "kiro" => ProviderLogo::fixed("icons/editors/kiro.svg"),
        "vscode" => ProviderLogo::fixed("icons/editors/vscode.svg"),
        "vscode-insiders" => ProviderLogo::fixed("icons/editors/vscode-insiders.svg"),
        "vscodium" => ProviderLogo::fixed("icons/editors/vscodium.svg"),
        "idea" => ProviderLogo::fixed("icons/editors/idea.svg"),
        "aqua" => ProviderLogo::fixed("icons/editors/aqua.svg"),
        "clion" => ProviderLogo::fixed("icons/editors/clion.svg"),
        "datagrip" => ProviderLogo::fixed("icons/editors/datagrip.svg"),
        "dataspell" => ProviderLogo::fixed("icons/editors/dataspell.svg"),
        "goland" => ProviderLogo::fixed("icons/editors/goland.svg"),
        "phpstorm" => ProviderLogo::fixed("icons/editors/phpstorm.svg"),
        "pycharm" => ProviderLogo::fixed("icons/editors/pycharm.svg"),
        "rider" => ProviderLogo::fixed("icons/editors/rider.svg"),
        "rubymine" => ProviderLogo::fixed("icons/editors/rubymine.svg"),
        "rustrover" => ProviderLogo::fixed("icons/editors/rustrover.svg"),
        "webstorm" => ProviderLogo::fixed("icons/editors/webstorm.svg"),
        _ => return None,
    })
}

/// The mark for the source control provider's wire kind.
#[allow(dead_code, reason = "Used by the source control controls when their callers are migrated")]
pub fn source_control_logo(kind: &str) -> Option<ProviderLogo> {
    Some(match kind {
        "azure-devops" => ProviderLogo::fixed("icons/source-control/azure-devops.svg"),
        "bitbucket" => ProviderLogo::fixed("icons/source-control/bitbucket.svg"),
        "cursor-origin" => ProviderLogo::Mono("icons/source-control/cursor-origin.svg"),
        "forgejo" => ProviderLogo::fixed("icons/source-control/forgejo.svg"),
        "gitcafe" => ProviderLogo::fixed("icons/source-control/gitcafe.svg"),
        "github" => ProviderLogo::Mono("icons/source-control/github.svg"),
        "gitlab" => ProviderLogo::fixed("icons/source-control/gitlab.svg"),
        _ => return None,
    })
}

/// Serves the vendored icons to GPUI.
pub struct Assets;

impl Assets {
    fn files() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
        FILES.iter().chain(PROVIDER_FILES).chain(BRAND_FILES)
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
