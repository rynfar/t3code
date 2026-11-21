//! Lucide icons vendored at the web's version (`lucide-react` 0.564.0) with the web's stroke
//! baked in (design system §8): 1.75 by default, 2 under `stroke-2/` for work-row icons. The
//! composer's own glyphs (send arrow, stop square, filled star and bolt) live under `composer/`,
//! provider brand marks under `providers/`, and the preview browser's own marks under `preview/`.
//! The asset source also serves gpui-component's own icons, which its `IconName` widgets load.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, Pixels, SharedString, Svg, prelude::*, svg};

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
