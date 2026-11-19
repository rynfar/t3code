//! Per-thread right panel state: which surfaces (tabs) are open, which is active, whether the
//! panel shows, narrow-window sheet sizing, proactive panel opening, and the closed-tab history
//! `cmd-shift-t` reopens. Ports `apps/web/src/rightPanelStore.ts`,
//! `apps/web/src/rightPanelLayout.ts`, `apps/web/src/components/RightPanelSheet.tsx`,
//! `apps/web/src/hooks/useProactivePanels.ts`, `apps/web/src/components/ChatView.logic.ts`,
//! `apps/web/src/closedViewStore.ts` (`pingdotgg/t3code#34859`), and `apps/web/src/reopenClosedView.ts`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::diff::TurnDiffSummary;
use crate::terminal::{MAX_TERMINALS_PER_GROUP, SplitDirection, terminal_label};

/// `PullRequestDetailTab` from `rightPanelStore.ts`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestDetailTab {
    #[default]
    Summary,
    Guide,
    Timeline,
    Code,
}

/// `ChatFileAttachment` metadata retained on attachment file tabs (`attachment:<id>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Input target for opening or matching a single pull-request right-panel tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub repository: String,
    pub number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_tab: Option<PullRequestDetailTab>,
}

/// `RightPanelSurface`: a tab. Singletons such as the diff use their kind as the id; surfaces
/// that can repeat (files, browsers, terminals, pull requests, devices) carry their resource in the id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Surface {
    pub id: String,
    #[serde(flatten)]
    pub kind: SurfaceKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SurfaceKind {
    Diff,
    PullRequests,
    /// A single pull request tab (`pull-request:<scope><projectId>:<host><repo>:<number>`).
    #[serde(rename_all = "camelCase")]
    PullRequest {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment_id: Option<String>,
        project_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<String>,
        repository: String,
        number: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reveal_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail_tab: Option<PullRequestDetailTab>,
    },
    /// The workspace explorer on its own, with optional breadcrumb return trail.
    #[serde(rename_all = "camelCase")]
    Files {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder_trail: Option<String>,
    },
    /// A file (or, until its read says otherwise, a folder) at a workspace-relative or host path,
    /// or a chat file attachment (`attachment:<id>`).
    /// `reveal_request_id` grows on every open so opening the same line again scrolls again.
    #[serde(rename_all = "camelCase")]
    File {
        relative_path: String,
        #[serde(default, deserialize_with = "reveal_line")]
        reveal_line: Option<u64>,
        #[serde(default)]
        reveal_request_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attachment: Option<FileAttachment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder_trail: Option<String>,
    },
    /// A right-panel terminal tab (`terminal:<id>`), supporting splits inside the tab.
    #[serde(rename_all = "camelCase")]
    Terminal {
        resource_id: String,
        terminal_ids: Vec<String>,
        active_terminal_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        split_direction: Option<SplitDirection>,
    },
    #[serde(rename_all = "camelCase")]
    SideChat { thread_id: String },
    /// A browser tab: `browser:<tab>` for a preview session, `browser:new` before one opens.
    #[serde(rename_all = "camelCase")]
    Preview { resource_id: Option<String> },
    /// A device tab: `device` while it lists devices, `device:<host>:<device>` once one opened.
    Device {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<DeviceTarget>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

/// `DeviceTabTarget`: the device a tab shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceTarget {
    pub host_id: String,
    pub device_id: String,
    pub platform: crate::device::Platform,
    pub name: String,
}

/// `normalizeRevealLine`: a positive whole line, anything else none.
fn reveal_line<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    Ok(Option::<Value>::deserialize(deserializer)?.and_then(|value| value.as_f64()).filter(|line| line.is_finite()).map(|line| line.trunc().max(1.) as u64))
}

/// `pullRequestSurfaceId` from `rightPanelStore.ts`.
pub fn pull_request_surface_id(
    environment_id: Option<&str>,
    project_id: &str,
    host: Option<&str>,
    repository: &str,
    number: u64,
) -> String {
    let scope = match environment_id {
        Some(env) => format!("{}:", crate::device::encode_component(env)),
        None => String::new(),
    };
    let host = match host {
        Some(host) => format!("{}:", crate::device::encode_component(&host.to_ascii_lowercase())),
        None => String::new(),
    };
    format!(
        "pull-request:{scope}{}:{host}{}:{number}",
        crate::device::encode_component(project_id),
        crate::device::encode_component(repository),
    )
}

impl Surface {
    pub fn diff() -> Surface {
        Surface { id: "diff".to_owned(), kind: SurfaceKind::Diff }
    }

    /// A thread's linked pull requests (`{ id: "pull-requests", kind: "pull-requests" }`).
    pub fn pull_requests() -> Surface {
        Surface { id: "pull-requests".to_owned(), kind: SurfaceKind::PullRequests }
    }

    /// A single pull request tab (`pullRequestSurface`).
    pub fn pull_request(target: PullRequestTarget) -> Surface {
        let host = target.host.map(|host| host.to_ascii_lowercase());
        let id = pull_request_surface_id(
            target.environment_id.as_deref(),
            &target.project_id,
            host.as_deref(),
            &target.repository,
            target.number,
        );
        Surface {
            id,
            kind: SurfaceKind::PullRequest {
                environment_id: target.environment_id,
                project_id: target.project_id,
                host,
                repository: target.repository,
                number: target.number,
                url: target.url,
                reveal_url: target.reveal_url,
                detail_tab: target.detail_tab,
            },
        }
    }

    pub fn files() -> Surface {
        Surface { id: "files".to_owned(), kind: SurfaceKind::Files { folder_trail: None } }
    }

    pub fn files_with_trail(folder_trail: Option<String>) -> Surface {
        Surface { id: "files".to_owned(), kind: SurfaceKind::Files { folder_trail } }
    }

    pub fn file(relative_path: &str, reveal_line: Option<u64>, reveal_request_id: u64) -> Surface {
        Surface {
            id: format!("file:{relative_path}"),
            kind: SurfaceKind::File {
                relative_path: relative_path.to_owned(),
                reveal_line,
                reveal_request_id,
                attachment: None,
                folder_trail: None,
            },
        }
    }

    pub fn file_with_trail(
        relative_path: &str,
        reveal_line: Option<u64>,
        reveal_request_id: u64,
        folder_trail: Option<String>,
    ) -> Surface {
        Surface {
            id: format!("file:{relative_path}"),
            kind: SurfaceKind::File {
                relative_path: relative_path.to_owned(),
                reveal_line,
                reveal_request_id,
                attachment: None,
                folder_trail,
            },
        }
    }

    /// `attachmentSurface` (`rightPanelStore.ts`).
    pub fn attachment(attachment: FileAttachment) -> Surface {
        Surface {
            id: format!("attachment:{}", attachment.id),
            kind: SurfaceKind::File {
                relative_path: attachment.name.clone(),
                reveal_line: None,
                reveal_request_id: 0,
                attachment: Some(attachment),
                folder_trail: None,
            },
        }
    }

    /// `terminalSurface` (`rightPanelStore.ts`).
    pub fn terminal(terminal_id: &str) -> Surface {
        Surface {
            id: format!("terminal:{terminal_id}"),
            kind: SurfaceKind::Terminal {
                resource_id: terminal_id.to_owned(),
                terminal_ids: vec![terminal_id.to_owned()],
                active_terminal_id: terminal_id.to_owned(),
                split_direction: None,
            },
        }
    }

    /// `side-chat:<thread>`, as `openSideChat` keys it.
    pub fn side_chat(thread_id: &str) -> Surface {
        Surface { id: format!("side-chat:{thread_id}"), kind: SurfaceKind::SideChat { thread_id: thread_id.to_owned() } }
    }

    /// `browserSurface`.
    pub fn browser(tab_id: Option<&str>) -> Surface {
        match tab_id {
            Some(tab_id) => Surface { id: format!("browser:{tab_id}"), kind: SurfaceKind::Preview { resource_id: Some(tab_id.to_owned()) } },
            None => Surface { id: BROWSER_PLACEHOLDER_ID.to_owned(), kind: SurfaceKind::Preview { resource_id: None } },
        }
    }

    /// The device tab: the `device` placeholder, or the one keyed by its device.
    pub fn device(target: Option<DeviceTarget>) -> Surface {
        let id = match &target {
            Some(target) => format!("device:{}:{}", crate::device::encode_component(&target.host_id), crate::device::encode_component(&target.device_id)),
            None => DEVICE_PLACEHOLDER_ID.to_owned(),
        };
        Surface { id, kind: SurfaceKind::Device { target, title: None } }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// `withoutReveal`: a pending link belongs to the click that made it, not to reload or reopen.
    pub fn without_reveal(&self) -> Surface {
        match &self.kind {
            SurfaceKind::PullRequest { reveal_url: Some(_), .. } => {
                let mut cloned = self.clone();
                if let SurfaceKind::PullRequest { reveal_url, .. } = &mut cloned.kind {
                    *reveal_url = None;
                }
                cloned
            }
            _ => self.clone(),
        }
    }

    /// The preview session a browser tab shows.
    pub fn preview_tab(&self) -> Option<&str> {
        match &self.kind {
            SurfaceKind::Preview { resource_id } => resource_id.as_deref(),
            _ => None,
        }
    }

    /// `surfaceTitle` in `RightPanelTabs.tsx`: a file tab shows its name, a PR shows `#<number>`.
    pub fn title(&self) -> String {
        match &self.kind {
            SurfaceKind::Diff => "Diff".to_owned(),
            SurfaceKind::PullRequests => "Pull requests".to_owned(),
            SurfaceKind::PullRequest { number, .. } => format!("#{number}"),
            SurfaceKind::Files { .. } => "Files".to_owned(),
            SurfaceKind::File { relative_path, .. } => {
                let start = relative_path.rfind(['/', '\\']).map_or(0, |index| index + 1);
                relative_path[start..].to_owned()
            }
            SurfaceKind::Terminal { active_terminal_id, .. } => terminal_label(active_terminal_id),
            SurfaceKind::SideChat { .. } => "Side chat".to_owned(),
            SurfaceKind::Preview { .. } => "Browser".to_owned(),
            SurfaceKind::Device { target, title } => title.clone().or_else(|| target.as_ref().map(|target| target.name.clone())).unwrap_or_else(|| "Device".to_owned()),
        }
    }

    pub fn side_chat_thread(&self) -> Option<&str> {
        match &self.kind {
            SurfaceKind::SideChat { thread_id } => Some(thread_id),
            _ => None,
        }
    }

    pub fn terminal_ids(&self) -> &[String] {
        match &self.kind {
            SurfaceKind::Terminal { terminal_ids, .. } => terminal_ids,
            _ => &[],
        }
    }

    pub fn folder_trail(&self) -> Option<&str> {
        match &self.kind {
            SurfaceKind::Files { folder_trail } | SurfaceKind::File { folder_trail, .. } => folder_trail.as_deref(),
            _ => None,
        }
    }

    pub fn is_attachment(&self) -> bool {
        matches!(&self.kind, SurfaceKind::File { attachment: Some(_), .. })
    }

    fn from_value(value: &Value) -> Option<Surface> {
        let obj = value.as_object()?;
        let kind = obj.get("kind")?.as_str()?;
        match kind {
            "plan" | "agents" => None,
            "terminal" => {
                let id = obj.get("id")?.as_str()?;
                let resource_id = obj.get("resourceId")?.as_str()?;
                if id != format!("terminal:{resource_id}") {
                    return None;
                }
                let mut seen = HashSet::new();
                let mut terminal_ids: Vec<String> = obj
                    .get("terminalIds")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .filter(|tid| seen.insert((*tid).to_owned()))
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                if terminal_ids.is_empty() {
                    terminal_ids.push(resource_id.to_owned());
                }
                let active_terminal_id = obj
                    .get("activeTerminalId")
                    .and_then(Value::as_str)
                    .filter(|tid| terminal_ids.iter().any(|entry| entry == tid))
                    .unwrap_or(&terminal_ids[0])
                    .to_owned();
                let split_direction = match obj.get("splitDirection").and_then(Value::as_str) {
                    Some("vertical") => Some(SplitDirection::Vertical),
                    _ => None,
                };
                Some(Surface {
                    id: id.to_owned(),
                    kind: SurfaceKind::Terminal {
                        resource_id: resource_id.to_owned(),
                        terminal_ids,
                        active_terminal_id,
                        split_direction,
                    },
                })
            }
            "pull-request" => {
                let project_id = obj.get("projectId")?.as_str()?.to_owned();
                let repository = obj.get("repository")?.as_str()?.to_owned();
                let number = obj.get("number")?.as_u64().filter(|n| *n >= 1)?;
                let environment_id = obj.get("environmentId").and_then(Value::as_str).map(str::to_owned);
                let host = obj.get("host").and_then(Value::as_str).map(str::to_owned);
                let url = obj.get("url").and_then(Value::as_str).map(str::to_owned);
                let detail_tab = obj.get("detailTab").and_then(|v| serde_json::from_value(v.clone()).ok());
                Some(Surface::pull_request(PullRequestTarget {
                    environment_id,
                    project_id,
                    host,
                    repository,
                    number,
                    url,
                    reveal_url: None,
                    detail_tab,
                }))
            }
            "files" | "file" => {
                let mut cleaned = obj.clone();
                if let Some(trail) = cleaned.get("folderTrail")
                    && !trail.is_string()
                {
                    cleaned.remove("folderTrail");
                }
                serde_json::from_value(Value::Object(cleaned)).ok()
            }
            _ => serde_json::from_value(value.clone()).ok(),
        }
    }
}

fn serialize_surfaces_without_reveal<S: serde::Serializer>(
    surfaces: &[Surface],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let cleaned: Vec<Surface> = surfaces.iter().map(Surface::without_reveal).collect();
    cleaned.serialize(serializer)
}

/// `ThreadRightPanelState`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPanel {
    pub is_open: bool,
    pub active_surface_id: Option<String>,
    #[serde(serialize_with = "serialize_surfaces_without_reveal")]
    pub surfaces: Vec<Surface>,
    pub maximized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactive_diff_turn_id: Option<String>,
}

impl ThreadPanel {
    /// The persisted-state migration in `rightPanelStore.ts`: surfaces this build does not know
    /// are dropped, and the active tab and visibility are repaired to match what is left.
    fn from_value(value: &Value) -> ThreadPanel {
        let surfaces: Vec<Surface> = value
            .get("surfaces")
            .and_then(Value::as_array)
            .map(|surfaces| surfaces.iter().filter_map(Surface::from_value).collect())
            .unwrap_or_default();
        let raw_active = value.get("activeSurfaceId").and_then(Value::as_str);
        let persisted_active = raw_active
            .filter(|id| surfaces.iter().any(|surface| surface.id == *id))
            .map(str::to_owned)
            .or_else(|| {
                (raw_active == Some("pull-request"))
                    .then(|| surfaces.iter().find(|s| matches!(s.kind, SurfaceKind::PullRequest { .. })).map(|s| s.id.clone()))
                    .flatten()
            });
        let is_open = !surfaces.is_empty()
            && value.get("isOpen").and_then(Value::as_bool).unwrap_or(persisted_active.is_some());
        let active_surface_id = persisted_active.or_else(|| if is_open { surfaces.first().map(|surface| surface.id.clone()) } else { None });
        let proactive_diff_turn_id = value.get("proactiveDiffTurnId").and_then(Value::as_str).map(str::to_owned);
        ThreadPanel {
            is_open,
            active_surface_id,
            surfaces,
            maximized: value.get("maximized").and_then(Value::as_bool).unwrap_or(false),
            proactive_diff_turn_id,
        }
    }
}

/// A map whose entries decode one by one: a bad entry is dropped, the rest survive.
pub fn decode_lenient_map<T: serde::de::DeserializeOwned>(value: Option<&Value>) -> HashMap<String, T> {
    value
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(key, entry)| serde_json::from_value(entry.clone()).ok().map(|entry| (key.clone(), entry)))
                .collect()
        })
        .unwrap_or_default()
}

impl ThreadPanel {
    pub fn active(&self) -> Option<&Surface> {
        let id = self.active_surface_id.as_deref()?;
        self.surfaces.iter().find(|surface| surface.id() == id)
    }

    fn is_empty(&self) -> bool {
        !self.is_open
            && self.active_surface_id.is_none()
            && self.surfaces.is_empty()
            && !self.maximized
            && self.proactive_diff_turn_id.is_none()
    }

    /// `upsertSurface`: adds the surface if missing, opens the panel, activates it.
    fn upsert(&mut self, surface: Surface) {
        self.is_open = true;
        self.active_surface_id = Some(surface.id().to_owned());
        if !self.surfaces.iter().any(|entry| entry.id == surface.id) {
            self.surfaces.push(surface);
        }
    }
}

/// `ClosedViewEntry`: a closed tab and the thread whose panel it belonged to.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClosedView {
    pub thread_id: String,
    #[serde(flatten)]
    pub target: ClosedTarget,
}

/// `ClosedView`'s two kinds: a panel tab reopens as itself; a browser tab's session closed with
/// it, so it reopens as a new session opened with the old one's settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ClosedTarget {
    PanelTab { surface: Surface },
    Browser { snapshot: BrowserSnapshot },
}

/// The `PreviewSessionSnapshot` fields `reopenClosedView` reopens a browser tab with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSnapshot {
    pub tab_id: String,
    /// The page, `None` for an idle tab (`navStatus._tag === "Idle"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom_factor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_scheme: Option<String>,
}

/// `INCOGNITO_BROWSER_PROFILE_ID` from `packages/contracts/src/browserProfile.ts`.
pub const INCOGNITO_BROWSER_PROFILE_ID: &str = "incognito";

impl BrowserSnapshot {
    pub fn from_session(snapshot: &crate::preview::SessionSnapshot) -> BrowserSnapshot {
        BrowserSnapshot {
            tab_id: snapshot.tab_id.clone(),
            url: snapshot.nav_status.url().map(str::to_owned),
            viewport: snapshot.viewport.clone(),
            profile_id: snapshot.profile_id.clone(),
            zoom_factor: snapshot.zoom_factor,
            color_scheme: snapshot.color_scheme.clone(),
        }
    }

    pub fn is_incognito(&self) -> bool {
        self.profile_id.as_deref() == Some(INCOGNITO_BROWSER_PROFILE_ID)
    }

    /// The `preview.open` input `openPreviewSession` sends for the server's browser: the old
    /// session's page and settings, each only when it had one.
    pub fn open_input(&self, thread_id: &str) -> Value {
        let mut input = serde_json::json!({ "threadId": thread_id, "runtime": "server" });
        let fields = [
            ("url", self.url.clone().map(Value::String)),
            ("viewport", self.viewport.clone()),
            ("profileId", self.profile_id.clone().map(Value::String)),
            ("zoomFactor", self.zoom_factor.and_then(serde_json::Number::from_f64).map(Value::Number)),
            ("colorScheme", self.color_scheme.clone().map(Value::String)),
        ];
        for (key, value) in fields {
            if let Some(value) = value {
                input[key] = value;
            }
        }
        input
    }

    /// Reopening a closed browser tab merges saved snapshot overrides over `BrowserDefaults`
    /// (`openPreviewSession.ts`, `pingdotgg/t3code#34943`).
    pub fn open_input_with_defaults(&self, thread_id: &str, defaults: &crate::preview::viewport::BrowserDefaults) -> Value {
        let overrides = crate::preview::viewport::OpenPreviewOverrides {
            url: self.url.clone(),
            viewport: self.viewport.as_ref().map(crate::preview::viewport::ViewportSetting::from_value),
            profile_id: self.profile_id.clone(),
            zoom_factor: self.zoom_factor,
            color_scheme: self.color_scheme.as_deref().and_then(crate::preview::viewport::AppearancePreference::parse),
        };
        crate::preview::viewport::open_session_payload(thread_id, defaults, &overrides)
    }
}

impl ClosedView {
    pub fn panel_tab(thread_id: &str, surface: Surface) -> ClosedView {
        ClosedView { thread_id: thread_id.to_owned(), target: ClosedTarget::PanelTab { surface: surface.without_reveal() } }
    }

    pub fn browser(thread_id: &str, snapshot: BrowserSnapshot) -> ClosedView {
        ClosedView { thread_id: thread_id.to_owned(), target: ClosedTarget::Browser { snapshot } }
    }

    pub fn surface(&self) -> Option<&Surface> {
        match &self.target {
            ClosedTarget::PanelTab { surface } => Some(surface),
            ClosedTarget::Browser { .. } => None,
        }
    }

    /// `sameTarget`: one entry per panel tab, or per browser session.
    fn same_target(&self, other: &ClosedView) -> bool {
        self.thread_id == other.thread_id
            && match (&self.target, &other.target) {
                (ClosedTarget::PanelTab { surface }, ClosedTarget::PanelTab { surface: other }) => surface.id == other.id,
                (ClosedTarget::Browser { snapshot }, ClosedTarget::Browser { snapshot: other }) => snapshot.tab_id == other.tab_id,
                _ => false,
            }
    }

    /// `isPersistentView`: an incognito tab is remembered for this run only.
    fn is_persistent(&self) -> bool {
        !matches!(&self.target, ClosedTarget::Browser { snapshot } if snapshot.is_incognito())
    }

    /// Entries saved before browser tabs were remembered carry no `kind`: they are panel tabs.
    fn from_value(value: &Value) -> Option<ClosedView> {
        let thread_id = value.get("threadId")?.as_str()?.to_owned();
        let target = match value.get("kind").and_then(Value::as_str).unwrap_or("panel-tab") {
            "panel-tab" => ClosedTarget::PanelTab { surface: Surface::from_value(value.get("surface")?)? },
            "browser" => ClosedTarget::Browser { snapshot: serde_json::from_value(value.get("snapshot")?.clone()).ok()? },
            _ => return None,
        };
        let view = ClosedView { thread_id, target };
        view.is_persistent().then_some(view)
    }
}

/// `partialize`: incognito entries never reach disk.
fn persistent_views<S: serde::Serializer>(views: &[ClosedView], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(views.iter().filter(|view| view.is_persistent()))
}

/// What `reopen_closed` restored, for the caller to finish.
#[derive(Debug, Clone, PartialEq)]
pub enum Reopened {
    /// A panel tab is back in this thread's panel; the caller selects the thread.
    Thread(String),
    /// A browser tab: the caller opens a session with `snapshot.open_input_with_defaults`, then
    /// `open_browser`s its tab, or hands the view back to `defer_closed` when that fails.
    Browser(ClosedView),
}

const CLOSED_VIEW_LIMIT: usize = 20;
const BROWSER_PLACEHOLDER_ID: &str = "browser:new";
const DEVICE_PLACEHOLDER_ID: &str = "device";

/// `usePreviewPanelInlineSize`: the inline panel's default and minimum width.
pub const DEFAULT_WIDTH: f32 = 540.;
pub const MIN_WIDTH: f32 = 360.;
/// The thread column beside the panel keeps at least this much, or its composer overflows.
const SIBLING_COLUMN_MIN_WIDTH: f32 = 360.;

/// `RIGHT_PANEL_INLINE_LAYOUT_MEDIA_QUERY = "(max-width: 980px)"` (`rightPanelLayout.ts`).
pub const SHEET_BREAKPOINT: f32 = 980.;
const SHEET_COMPACT_BREAKPOINT: f32 = 760.;

/// Below or at `SHEET_BREAKPOINT` the right panel renders as an overlay sheet (`RightPanelSheet.tsx`).
pub fn is_sheet_mode(viewport_width: f32) -> bool {
    viewport_width.is_finite() && viewport_width <= SHEET_BREAKPOINT
}

/// Sheet width matching `RightPanelSheet.tsx`:
/// `w-[min(42vw,28rem)] min-w-80 max-w-[28rem] max-[760px]:w-[min(88vw,24rem)] max-[760px]:min-w-0`.
pub fn sheet_width(viewport_width: f32) -> f32 {
    if !viewport_width.is_finite() || viewport_width <= 0. {
        return 320.;
    }
    if viewport_width <= SHEET_COMPACT_BREAKPOINT {
        (viewport_width * 0.88).min(384.).floor()
    } else {
        (viewport_width * 0.42).clamp(320., 448.).floor()
    }
}

/// `getPreviewPanelMaxWidth`: 70% of the window and whatever leaves the thread its minimum,
/// never below the minimum width (an inverted clamp would overwrite the saved width).
pub fn clamp_width(width: f32, viewport_width: f32, row_width: f32) -> f32 {
    let max = (viewport_width * 0.7).floor().min(row_width.floor() - SIBLING_COLUMN_MIN_WIDTH).max(MIN_WIDTH);
    width.clamp(MIN_WIDTH, max)
}

/// `breadcrumbPathOf` (`components/files/filePath.ts`): canonical crumb path spelling.
pub fn breadcrumb_path_of(relative_path: &str) -> String {
    crate::files::breadcrumbs("", relative_path)
        .last()
        .map(|crumb| crumb.path.clone())
        .unwrap_or_else(|| relative_path.to_owned())
}

/// `retainedBreadcrumbTrail` (`components/files/filePath.ts`): retain descendant crumbs when
/// navigating up; other navigation resets the trail to `path`.
pub fn retained_breadcrumb_trail(trail: Option<&str>, path: &str) -> String {
    let Some(trail) = trail else { return path.to_owned() };
    let target = breadcrumb_path_of(path);
    let contains = crate::files::breadcrumbs("", trail).iter().any(|crumb| crumb.path == target);
    if contains { trail.to_owned() } else { path.to_owned() }
}

/// Request accepted by `RightPanels::open_proactive` (`rightPanelStore.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProactiveRequest {
    PullRequest(PullRequestTarget),
    PullRequests,
    Diff { turn_id: String },
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RightPanels {
    threads: HashMap<String, ThreadPanel>,
    /// `t3code:preview-panel-width:<thread>`.
    widths: HashMap<String, f32>,
    /// Newest first.
    #[serde(serialize_with = "persistent_views")]
    closed: Vec<ClosedView>,
    /// Runtime-only counter bumped on every explicit user panel action (`userActionRevisionByThreadKey`).
    #[serde(skip)]
    user_action_revisions: HashMap<String, u64>,
}

impl PartialEq for RightPanels {
    fn eq(&self, other: &Self) -> bool {
        self.threads == other.threads && self.widths == other.widths && self.closed == other.closed
    }
}

impl RightPanels {
    /// Decodes persisted state without trusting it: every part that fails to decode falls back
    /// on its own, so one bad entry never costs the user their other tabs.
    pub fn from_value(value: &Value) -> RightPanels {
        let threads = value
            .get("threads")
            .and_then(Value::as_object)
            .map(|threads| {
                threads
                    .iter()
                    .map(|(thread, panel)| (thread.clone(), ThreadPanel::from_value(panel)))
                    .filter(|(_, panel)| !panel.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let closed = value
            .get("closed")
            .and_then(Value::as_array)
            .map(|closed| closed.iter().filter_map(ClosedView::from_value).take(CLOSED_VIEW_LIMIT).collect())
            .unwrap_or_default();
        let widths = decode_lenient_map::<f32>(value.get("widths")).into_iter().filter(|(_, width)| width.is_finite()).collect();
        RightPanels { threads, widths, closed, user_action_revisions: HashMap::new() }
    }
}

impl<'de> Deserialize<'de> for RightPanels {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(RightPanels::from_value(&Value::deserialize(deserializer)?))
    }
}

static EMPTY: ThreadPanel = ThreadPanel {
    is_open: false,
    active_surface_id: None,
    surfaces: Vec::new(),
    maximized: false,
    proactive_diff_turn_id: None,
};

impl RightPanels {
    pub fn panel(&self, thread_id: &str) -> &ThreadPanel {
        self.threads.get(thread_id).unwrap_or(&EMPTY)
    }

    /// `selectActiveRightPanelSurface`: the active surface while the panel shows.
    pub fn active_surface(&self, thread_id: &str) -> Option<&Surface> {
        let panel = self.panel(thread_id);
        if panel.is_open { panel.active() } else { None }
    }

    /// All terminal IDs currently owned by right-panel terminal tabs of `thread_id`.
    pub fn panel_terminal_ids(&self, thread_id: &str) -> HashSet<String> {
        self.panel(thread_id)
            .surfaces
            .iter()
            .flat_map(|surface| surface.terminal_ids().iter().cloned())
            .collect()
    }

    /// The thread the visible panel needs beside the selected thread.
    pub fn active_thread_source<'a>(&'a self, thread_id: &str, selection: &'a crate::diff::DiffSelection) -> Option<&'a str> {
        match &self.active_surface(thread_id)?.kind {
            SurfaceKind::SideChat { thread_id } => Some(thread_id),
            SurfaceKind::Diff => match selection {
                crate::diff::DiffSelection::ToolCall(target) => Some(&target.thread_id),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn is_open(&self, thread_id: &str) -> bool {
        self.panel(thread_id).is_open
    }

    pub fn width(&self, thread_id: &str) -> Option<f32> {
        self.widths.get(thread_id).copied()
    }

    pub fn closed_views(&self) -> &[ClosedView] {
        &self.closed
    }

    pub fn get_user_action_revision(&self, thread_id: &str) -> u64 {
        self.user_action_revisions.get(thread_id).copied().unwrap_or(0)
    }

    pub fn mark_user_action(&mut self, thread_id: &str) -> u64 {
        let entry = self.user_action_revisions.entry(thread_id.to_owned()).or_insert(0);
        *entry += 1;
        *entry
    }

    pub fn set_width(&mut self, thread_id: &str, width: f32) -> bool {
        if self.widths.get(thread_id) == Some(&width) {
            return false;
        }
        self.widths.insert(thread_id.to_owned(), width);
        true
    }

    /// Applies `update` without bumping `user_action_revisions` (`automaticUpdate`).
    fn update_auto(&mut self, thread_id: &str, update: impl FnOnce(&mut ThreadPanel)) -> bool {
        let before = self.panel(thread_id).clone();
        let mut next = before.clone();
        update(&mut next);
        if next == before {
            return false;
        }
        if next.is_empty() {
            self.threads.remove(thread_id);
        } else {
            self.threads.insert(thread_id.to_owned(), next);
        }
        true
    }

    /// Applies `update` as an explicit user action, bumping `user_action_revisions` (`userActionOnThread`).
    fn update_user(&mut self, thread_id: &str, update: impl FnOnce(&mut ThreadPanel)) -> bool {
        self.mark_user_action(thread_id);
        self.update_auto(thread_id, update)
    }

    /// Remembers what a close removed: the tabs that were not active first, then the active one,
    /// so reopening brings back the tab the user was looking at first.
    fn close(&mut self, thread_id: &str, update: impl FnOnce(&mut ThreadPanel)) -> bool {
        let before = self.panel(thread_id).clone();
        if !self.update_user(thread_id, update) {
            return false;
        }
        let after = self.panel(thread_id).surfaces.clone();
        let active = before.active_surface_id.as_deref();
        let removed: Vec<&Surface> =
            before.surfaces.iter().filter(|surface| !after.iter().any(|entry| entry.id == surface.id)).collect();
        let ordered = removed
            .iter()
            .filter(|surface| Some(surface.id()) != active)
            .chain(removed.iter().filter(|surface| Some(surface.id()) == active));
        // Terminal sessions and server browser tabs close with their tabs; don't push them as panel-tabs.
        for surface in ordered
            .filter(|surface| surface.preview_tab().is_none() && !matches!(surface.kind, SurfaceKind::Terminal { .. }))
        {
            self.remember(ClosedView::panel_tab(thread_id, surface.without_reveal()));
        }
        true
    }

    /// `closedViewStore.remember`: newest first, one entry per target, at most 20.
    fn remember(&mut self, view: ClosedView) {
        self.closed.retain(|entry| !entry.same_target(&view));
        self.closed.insert(0, view);
        self.closed.truncate(CLOSED_VIEW_LIMIT);
    }

    /// `closePreviewSession`: a browser tab the user closed, remembered with its session's
    /// snapshot so `cmd-shift-t` can open the page again.
    pub fn remember_browser(&mut self, thread_id: &str, snapshot: BrowserSnapshot) {
        self.remember(ClosedView::browser(thread_id, snapshot));
    }

    /// `closedViewStore.defer`: a view that failed to reopen goes to the back of the history
    /// instead of being lost, unless newer closes filled it meanwhile.
    pub fn defer_closed(&mut self, view: ClosedView) {
        self.closed.retain(|entry| !entry.same_target(&view));
        if self.closed.len() < CLOSED_VIEW_LIMIT {
            self.closed.push(view);
        }
    }

    /// `openProactive` (`rightPanelStore.ts:663-702`): opens a proactive PR or Diff surface only
    /// if the user has not made a choice since `expected_user_action_revision`.
    pub fn open_proactive(
        &mut self,
        thread_id: &str,
        request: ProactiveRequest,
        expected_user_action_revision: u64,
        activate_existing: bool,
    ) -> bool {
        let current_rev = self.get_user_action_revision(thread_id);
        match request {
            ProactiveRequest::Diff { turn_id } => {
                if self.panel(thread_id).proactive_diff_turn_id.as_deref() == Some(turn_id.as_str()) {
                    return false;
                }
                let active_kind = self.active_surface(thread_id).map(|surface| &surface.kind);
                let pull_request_active = matches!(active_kind, Some(SurfaceKind::PullRequest { .. } | SurfaceKind::PullRequests));
                let diff_active = matches!(active_kind, Some(SurfaceKind::Diff));
                let opened = current_rev == expected_user_action_revision && !diff_active && !pull_request_active;
                // Record the offer even when refused, so a revisit does not retry this turn.
                self.update_auto(thread_id, |panel| {
                    if opened {
                        panel.upsert(Surface::diff());
                    }
                    panel.proactive_diff_turn_id = Some(turn_id);
                });
                opened
            }
            ProactiveRequest::PullRequests | ProactiveRequest::PullRequest(_) => {
                if current_rev != expected_user_action_revision {
                    return false;
                }
                let surface = match request {
                    ProactiveRequest::PullRequests => Surface::pull_requests(),
                    ProactiveRequest::PullRequest(target) => Surface::pull_request(target),
                    ProactiveRequest::Diff { .. } => unreachable!(),
                };
                if !activate_existing && self.panel(thread_id).surfaces.iter().any(|entry| entry.id == surface.id) {
                    return false;
                }
                self.update_auto(thread_id, |panel| panel.upsert(surface));
                true
            }
        }
    }

    pub fn open(&mut self, thread_id: &str, surface: Surface) -> bool {
        self.update_user(thread_id, |panel| panel.upsert(surface))
    }

    /// `toggle`: hides the panel when the surface is already in front, otherwise opens it.
    pub fn toggle(&mut self, thread_id: &str, surface: Surface) -> bool {
        self.update_user(thread_id, |panel| {
            if panel.is_open && panel.active().is_some_and(|active| active.id == surface.id) {
                panel.is_open = false;
            } else {
                panel.upsert(surface);
            }
        })
    }

    pub fn toggle_visibility(&mut self, thread_id: &str) -> bool {
        self.update_user(thread_id, |panel| panel.is_open = !panel.is_open)
    }

    /// `show`: the panel opens, on the launcher when it has no tab.
    pub fn show(&mut self, thread_id: &str) -> bool {
        self.update_user(thread_id, |panel| panel.is_open = true)
    }

    pub fn hide(&mut self, thread_id: &str) -> bool {
        self.update_user(thread_id, |panel| panel.is_open = false)
    }

    pub fn activate(&mut self, thread_id: &str, surface_id: &str) -> bool {
        self.update_user(thread_id, |panel| {
            if panel.surfaces.iter().any(|surface| surface.id() == surface_id) {
                panel.is_open = true;
                panel.active_surface_id = Some(surface_id.to_owned());
            }
        })
    }

    /// `closeSurface`: the tab at the same position, or the new last one, becomes active.
    pub fn close_surface(&mut self, thread_id: &str, surface_id: &str) -> bool {
        self.close(thread_id, |panel| {
            let Some(index) = panel.surfaces.iter().position(|surface| surface.id() == surface_id) else { return };
            panel.surfaces.remove(index);
            panel.is_open = panel.is_open && !panel.surfaces.is_empty();
            if panel.active_surface_id.as_deref() == Some(surface_id) {
                let fallback = panel.surfaces.get(index.min(panel.surfaces.len().saturating_sub(1)));
                panel.active_surface_id = fallback.map(|surface| surface.id().to_owned());
            }
        })
    }

    pub fn close_other_surfaces(&mut self, thread_id: &str, surface_id: &str) -> bool {
        self.close(thread_id, |panel| {
            let Some(surface) = panel.surfaces.iter().find(|surface| surface.id() == surface_id).cloned() else { return };
            if panel.surfaces.len() == 1 {
                return;
            }
            panel.is_open = true;
            panel.active_surface_id = Some(surface.id().to_owned());
            panel.surfaces = vec![surface];
        })
    }

    pub fn close_surfaces_to_right(&mut self, thread_id: &str, surface_id: &str) -> bool {
        self.close(thread_id, |panel| {
            let Some(index) = panel.surfaces.iter().position(|surface| surface.id() == surface_id) else { return };
            panel.surfaces.truncate(index + 1);
            let active_kept = panel.active().is_some();
            if !active_kept {
                panel.active_surface_id = Some(surface_id.to_owned());
            }
        })
    }

    pub fn close_all_surfaces(&mut self, thread_id: &str) -> bool {
        self.close(thread_id, |panel| {
            if !panel.surfaces.is_empty() {
                panel.is_open = false;
                panel.surfaces.clear();
                panel.active_surface_id = None;
            }
        })
    }

    pub fn move_surface(&mut self, thread_id: &str, surface_id: &str, to_index: usize) -> bool {
        self.update_auto(thread_id, |panel| {
            let Some(from) = panel.surfaces.iter().position(|surface| surface.id() == surface_id) else { return };
            let surface = panel.surfaces.remove(from);
            panel.surfaces.insert(to_index.min(panel.surfaces.len()), surface);
        })
    }

    /// `openFile`: `None` opens the explorer; a path replaces the standalone explorer with its
    /// file tab (which carries the explorer beside it) and asks for a fresh reveal.
    pub fn open_file(&mut self, thread_id: &str, path: Option<&str>, line: Option<u64>) -> bool {
        let Some(path) = path else { return self.open(thread_id, Surface::files()) };
        self.update_user(thread_id, |panel| {
            panel.surfaces.retain(|surface| !matches!(surface.kind, SurfaceKind::Files { .. }));
            let id = format!("file:{path}");
            let previous = panel.surfaces.iter().find_map(|surface| match &surface.kind {
                SurfaceKind::File { reveal_request_id, .. } if surface.id == id => Some(*reveal_request_id),
                _ => None,
            });
            let surface = Surface::file(path, line.map(|line| line.max(1)), previous.unwrap_or(0) + 1);
            panel.is_open = true;
            panel.active_surface_id = Some(id.clone());
            match panel.surfaces.iter_mut().find(|entry| entry.id == id) {
                Some(existing) => *existing = surface,
                None => panel.surfaces.push(surface),
            }
        })
    }

    /// `openAttachment`: drops the standalone explorer and opens an attachment tab (`attachment:<id>`).
    pub fn open_attachment(&mut self, thread_id: &str, attachment: FileAttachment) -> bool {
        self.update_user(thread_id, |panel| {
            panel.surfaces.retain(|surface| !matches!(surface.kind, SurfaceKind::Files { .. }));
            panel.upsert(Surface::attachment(attachment));
        })
    }

    /// `navigateFolder`: replaces `surface_id` with `folder` (`""` is the root explorer) while
    /// retaining `folder_trail` (unless equal to `folder`), or opens it beside the source when
    /// `keep_source`.
    pub fn navigate_folder_with_trail(
        &mut self,
        thread_id: &str,
        surface_id: &str,
        folder: &str,
        folder_trail: &str,
        keep_source: bool,
    ) -> bool {
        self.update_user(thread_id, |panel| {
            let Some(index) = panel.surfaces.iter().position(|surface| surface.id == surface_id) else { return };
            let trail = (folder_trail != folder).then(|| folder_trail.to_owned());
            let target = if folder.is_empty() {
                Surface::files_with_trail(trail)
            } else {
                Surface::file_with_trail(folder, None, 1, trail)
            };
            let exists = panel.surfaces.iter().enumerate().any(|(i, surface)| i != index && surface.id == target.id);
            let source = panel.surfaces[index].clone();
            let mut surfaces = Vec::with_capacity(panel.surfaces.len() + 1);
            for (i, surface) in panel.surfaces.drain(..).enumerate() {
                if i == index {
                    if keep_source {
                        surfaces.push(source.clone());
                    }
                    if !exists {
                        surfaces.push(target.clone());
                    }
                } else if exists || !matches!(target.kind, SurfaceKind::File { .. }) || !matches!(surface.kind, SurfaceKind::Files { .. }) {
                    surfaces.push(surface);
                }
            }
            panel.surfaces = surfaces;
            panel.is_open = true;
            panel.active_surface_id = Some(target.id);
        })
    }

    /// Convenience wrapper when no distinct descendant trail is carried.
    pub fn navigate_folder(&mut self, thread_id: &str, surface_id: &str, folder: &str, keep_source: bool) -> bool {
        self.navigate_folder_with_trail(thread_id, surface_id, folder, folder, keep_source)
    }

    /// `reconcileFileSurfaces`: without a workspace, workspace file tabs and the explorer close,
    /// while attachment tabs stay open (`rightPanelStore.ts:1079-1081`).
    pub fn close_file_surfaces(&mut self, thread_id: &str) -> bool {
        self.update_auto(thread_id, |panel| {
            let before = panel.surfaces.len();
            panel.surfaces.retain(|surface| match &surface.kind {
                SurfaceKind::Files { .. } => false,
                SurfaceKind::File { attachment, .. } => attachment.is_some(),
                _ => true,
            });
            if panel.surfaces.len() == before {
                return;
            }
            if panel.surfaces.is_empty() {
                panel.is_open = false;
            }
            if panel.active().is_none() {
                panel.active_surface_id = panel.surfaces.last().map(|surface| surface.id.clone());
            }
        })
    }

    /// `openPullRequest` (`rightPanelStore.ts:763-780`).
    pub fn open_pull_request(&mut self, thread_id: &str, target: PullRequestTarget) -> bool {
        let reveal_url = target.reveal_url.clone();
        let has_url_or_reveal = target.url.is_some() || reveal_url.is_some();
        let surface = Surface::pull_request(target);
        let id = surface.id.clone();
        self.update_user(thread_id, |panel| {
            panel.upsert(surface.clone());
            if !has_url_or_reveal {
                return;
            }
            if let Some(entry) = panel.surfaces.iter_mut().find(|entry| entry.id == id) {
                let mut updated = surface;
                if let SurfaceKind::PullRequest { reveal_url: dest, .. } = &mut updated.kind {
                    *dest = reveal_url;
                }
                *entry = updated;
            }
        })
    }

    /// `setPullRequestDetailTab` (`rightPanelStore.ts:781-797`).
    pub fn set_pull_request_detail_tab(&mut self, thread_id: &str, surface_id: &str, detail_tab: PullRequestDetailTab) -> bool {
        self.update_auto(thread_id, |panel| {
            if let Some(surface) = panel.surfaces.iter_mut().find(|surface| surface.id == surface_id)
                && let SurfaceKind::PullRequest { detail_tab: current, .. } = &mut surface.kind
                && current.unwrap_or_default() != detail_tab
            {
                *current = Some(detail_tab);
            }
        })
    }

    /// `settlePullRequestReveal` (`rightPanelStore.ts:798-811`).
    pub fn settle_pull_request_reveal(&mut self, thread_id: &str, surface_id: &str, expected_url: &str) -> bool {
        self.update_auto(thread_id, |panel| {
            if let Some(surface) = panel.surfaces.iter_mut().find(|surface| surface.id == surface_id)
                && let SurfaceKind::PullRequest { reveal_url, .. } = &mut surface.kind
                && reveal_url.as_deref() == Some(expected_url)
            {
                *reveal_url = None;
            }
        })
    }

    /// `openTerminal` (`rightPanelStore.ts:885-890`).
    pub fn open_terminal(&mut self, thread_id: &str, terminal_id: &str) -> bool {
        self.update_user(thread_id, |panel| panel.upsert(Surface::terminal(terminal_id)))
    }

    /// `splitTerminal` (`rightPanelStore.ts:891-910`).
    pub fn split_terminal(&mut self, thread_id: &str, surface_id: &str, terminal_id: &str, direction: SplitDirection) -> bool {
        self.update_user(thread_id, |panel| {
            let Some(surface) = panel.surfaces.iter_mut().find(|surface| surface.id == surface_id) else { return };
            let SurfaceKind::Terminal { terminal_ids, active_terminal_id, split_direction, .. } = &mut surface.kind else {
                return;
            };
            if !terminal_ids.iter().any(|id| id == terminal_id) {
                if terminal_ids.len() >= MAX_TERMINALS_PER_GROUP {
                    return;
                }
                terminal_ids.push(terminal_id.to_owned());
            }
            *active_terminal_id = terminal_id.to_owned();
            *split_direction = (direction == SplitDirection::Vertical).then_some(SplitDirection::Vertical);
            panel.is_open = true;
            panel.active_surface_id = Some(surface_id.to_owned());
        })
    }

    /// `activateTerminal` (`rightPanelStore.ts:911-924`).
    pub fn activate_terminal(&mut self, thread_id: &str, surface_id: &str, terminal_id: &str) -> bool {
        self.update_user(thread_id, |panel| {
            if let Some(surface) = panel.surfaces.iter_mut().find(|surface| surface.id == surface_id)
                && let SurfaceKind::Terminal { terminal_ids, active_terminal_id, .. } = &mut surface.kind
                && terminal_ids.iter().any(|id| id == terminal_id)
            {
                *active_terminal_id = terminal_id.to_owned();
                panel.active_surface_id = Some(surface_id.to_owned());
            }
        })
    }

    /// `closeTerminal` (`rightPanelStore.ts:925-963`): removes `terminal_id` from a terminal tab,
    /// closing the tab when it was the last terminal in it.
    pub fn close_terminal(&mut self, thread_id: &str, surface_id: &str, terminal_id: &str) -> bool {
        self.close(thread_id, |panel| {
            let Some(index) = panel.surfaces.iter().position(|surface| surface.id == surface_id) else { return };
            let remove_tab = match &mut panel.surfaces[index].kind {
                SurfaceKind::Terminal { terminal_ids, active_terminal_id, .. } => {
                    if !terminal_ids.iter().any(|id| id == terminal_id) {
                        return;
                    }
                    terminal_ids.retain(|id| id != terminal_id);
                    if terminal_ids.is_empty() {
                        true
                    } else {
                        if active_terminal_id == terminal_id {
                            *active_terminal_id = terminal_ids.last().cloned().unwrap_or_default();
                        }
                        false
                    }
                }
                _ => return,
            };
            if remove_tab {
                panel.surfaces.remove(index);
                panel.is_open = panel.is_open && !panel.surfaces.is_empty();
                if panel.active_surface_id.as_deref() == Some(surface_id) {
                    let fallback = panel.surfaces.get(index.min(panel.surfaces.len().saturating_sub(1)));
                    panel.active_surface_id = fallback.map(|surface| surface.id().to_owned());
                }
            }
        })
    }

    /// `openBrowser`: a session's tab replaces the `browser:new` placeholder.
    pub fn open_browser(&mut self, thread_id: &str, tab_id: Option<&str>) -> bool {
        self.update_user(thread_id, |panel| {
            if tab_id.is_some() {
                panel.surfaces.retain(|surface| surface.id != BROWSER_PLACEHOLDER_ID);
            }
            panel.upsert(Surface::browser(tab_id));
        })
    }

    /// `openDevice`: an opened device's tab takes the `device` placeholder's place, or, when the
    /// device already has a tab, the placeholder goes and that tab comes to the front.
    pub fn open_device(&mut self, thread_id: &str, target: DeviceTarget) -> bool {
        let surface = Surface::device(Some(target));
        self.update_user(thread_id, |panel| {
            let existing = panel.surfaces.iter().any(|entry| entry.id == surface.id);
            if existing {
                panel.surfaces.retain(|entry| entry.id != DEVICE_PLACEHOLDER_ID);
            } else if let Some(placeholder) = panel.surfaces.iter_mut().find(|entry| entry.id == DEVICE_PLACEHOLDER_ID) {
                *placeholder = surface.clone();
            }
            panel.upsert(surface);
        })
    }

    /// `reconcileBrowserSurfaces`: browser tabs follow the thread's preview sessions. Closed
    /// sessions lose their tab, new ones gain one unless `hidden`, and the placeholder goes.
    pub fn reconcile_browser_surfaces(&mut self, thread_id: &str, tab_ids: &[String], hidden: &HashSet<String>) -> bool {
        self.update_auto(thread_id, |panel| {
            let had_session_tab = panel.surfaces.iter().any(|surface| surface.preview_tab().is_some());
            panel.surfaces.retain(|surface| match &surface.kind {
                SurfaceKind::Preview { resource_id: Some(tab) } => tab_ids.contains(tab),
                SurfaceKind::Preview { resource_id: None } => false,
                _ => true,
            });
            for tab in tab_ids {
                if !hidden.contains(tab) && !panel.surfaces.iter().any(|surface| surface.preview_tab() == Some(tab.as_str())) {
                    panel.surfaces.push(Surface::browser(Some(tab)));
                }
            }
            // The last session tab closed: hide the panel as a user close would.
            if panel.surfaces.is_empty() && had_session_tab {
                panel.is_open = false;
            }
            if panel.active().is_none() {
                let fallback = panel.surfaces.iter().find(|surface| matches!(surface.kind, SurfaceKind::Preview { .. })).or(panel.surfaces.first());
                panel.active_surface_id = fallback.map(|surface| surface.id.clone());
            }
        })
    }

    pub fn set_maximized(&mut self, thread_id: &str, maximized: bool) -> bool {
        self.update_auto(thread_id, |panel| panel.maximized = maximized)
    }

    /// `planNextReopen` + `reopenClosedView`, newest first: an entry whose thread (or side chat)
    /// is gone is skipped once the shell is live, and stops the search while it still loads, since
    /// the thread may yet arrive; an entry already showing is dropped. The first other entry is
    /// restored. Files without a workspace cannot be, so that entry goes to the back
    /// (`defer`) and nothing reopens this time.
    pub fn reopen_closed(&mut self, thread_exists: impl Fn(&str) -> bool, shell_live: bool, workspace_available: impl Fn(&str) -> bool) -> Option<Reopened> {
        let mut index = 0;
        while index < self.closed.len() {
            let entry = &self.closed[index];
            let side_chat = entry.surface().and_then(Surface::side_chat_thread);
            if !thread_exists(&entry.thread_id) || side_chat.is_some_and(|id| !thread_exists(id)) {
                if !shell_live {
                    return None;
                }
                index += 1;
                continue;
            }
            let panel = self.panel(&entry.thread_id);
            let already_open = match &entry.target {
                ClosedTarget::PanelTab { surface } => panel.is_open && panel.surfaces.iter().any(|open| open.id == surface.id),
                ClosedTarget::Browser { snapshot } => panel.surfaces.iter().any(|open| open.preview_tab() == Some(snapshot.tab_id.as_str())),
            };
            if already_open {
                self.closed.remove(index);
                continue;
            }
            let entry = self.closed.remove(index);
            let ClosedTarget::PanelTab { surface } = &entry.target else { return Some(Reopened::Browser(entry)) };
            if self.restore_surface(&entry.thread_id, surface.clone(), workspace_available(&entry.thread_id)) {
                return Some(Reopened::Thread(entry.thread_id));
            }
            self.defer_closed(entry);
            return None;
        }
        None
    }

    /// The panel-tab arm of `reopenClosedView`: each kind reopens through its own open, so a file
    /// asks for a fresh reveal, folder trails survive, attachments reopen without a workspace,
    /// and a device takes the list's place with its name back.
    fn restore_surface(&mut self, thread_id: &str, surface: Surface, workspace_available: bool) -> bool {
        match surface.kind {
            SurfaceKind::Files { .. } if !workspace_available => false,
            SurfaceKind::File { attachment: None, .. } if !workspace_available => false,
            SurfaceKind::Preview { resource_id: Some(_) } => false,
            SurfaceKind::Preview { resource_id: None } => {
                self.open_browser(thread_id, None);
                true
            }
            SurfaceKind::Files { folder_trail } => {
                let root_already_exists = self.panel(thread_id).surfaces.iter().any(|s| matches!(s.kind, SurfaceKind::Files { .. }));
                self.open(thread_id, Surface::files());
                if !root_already_exists
                    && let Some(trail) = folder_trail
                {
                    self.navigate_folder_with_trail(thread_id, "files", "", &trail, false);
                }
                true
            }
            SurfaceKind::File { relative_path, reveal_line, attachment, folder_trail, .. } => {
                if let Some(attachment) = attachment {
                    self.open_attachment(thread_id, attachment);
                } else {
                    self.open_file(thread_id, Some(&relative_path), reveal_line);
                    if let Some(trail) = folder_trail {
                        let id = format!("file:{relative_path}");
                        self.navigate_folder_with_trail(thread_id, &id, &relative_path, &trail, false);
                    }
                }
                true
            }
            SurfaceKind::PullRequest { environment_id, project_id, host, repository, number, url, detail_tab, .. } => {
                self.open_pull_request(
                    thread_id,
                    PullRequestTarget { environment_id, project_id, host, repository, number, url, reveal_url: None, detail_tab },
                );
                true
            }
            SurfaceKind::Device { target: Some(target), title } => {
                let id = Surface::device(Some(target.clone())).id;
                self.open_device(thread_id, target);
                if let Some(title) = title {
                    self.rename_device(thread_id, &id, &title);
                }
                true
            }
            _ => {
                self.open(thread_id, surface);
                true
            }
        }
    }

    /// `renameDevice`: a blank name falls back to the device's own, then "Device".
    pub fn rename_device(&mut self, thread_id: &str, surface_id: &str, title: &str) -> bool {
        self.update_user(thread_id, |panel| {
            let Some(surface) = panel.surfaces.iter_mut().find(|surface| surface.id == surface_id) else { return };
            if let SurfaceKind::Device { target, title: current } = &mut surface.kind {
                let trimmed = title.trim();
                let name = if trimmed.is_empty() { target.as_ref().map_or("Device", |target| target.name.as_str()) } else { trimmed };
                *current = Some(name.to_owned());
            }
        })
    }

    pub fn remove_thread(&mut self, thread_id: &str) -> bool {
        let removed = self.threads.remove(thread_id).is_some();
        self.user_action_revisions.remove(thread_id);
        self.widths.remove(thread_id).is_some() || removed
    }
}

/// `ProactivePanelObservation` from `apps/web/src/components/ChatView.logic.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProactivePanelObservation {
    pub thread_key: String,
    /// `None` before the first settled observation on this thread; `Some(None)` when observed idle.
    pub running_turn_id: Option<Option<String>>,
    /// `None` before the first observation on this thread; `Some(None)` when observed with no linked PR.
    pub target_key: Option<Option<String>>,
    pub user_action_turn_id: Option<String>,
    pub user_action_revision: u64,
}

/// `shouldOpenProactivePullRequest` (`ChatView.logic.ts:156`).
pub fn should_open_proactive_pull_request(
    previous_target_key: Option<&Option<String>>,
    target_key: Option<&str>,
) -> bool {
    let Some(target) = target_key else { return false };
    previous_target_key.and_then(|opt| opt.as_deref()) != Some(target)
}

/// `observeProactivePanelUserChoice` (`ChatView.logic.ts:171`).
pub fn observe_proactive_panel_user_choice(
    previous: Option<&ProactivePanelObservation>,
    thread_key: &str,
    running_turn_id: Option<&str>,
    user_action_revision: u64,
) -> ProactivePanelObservation {
    let same_thread = previous.is_some_and(|prev| prev.thread_key == thread_key);
    let new_turn = same_thread
        && running_turn_id.is_some()
        && running_turn_id != previous.and_then(|prev| prev.user_action_turn_id.as_deref());
    ProactivePanelObservation {
        thread_key: thread_key.to_owned(),
        running_turn_id: if same_thread { previous.and_then(|prev| prev.running_turn_id.clone()) } else { None },
        target_key: if same_thread { previous.and_then(|prev| prev.target_key.clone()) } else { None },
        user_action_turn_id: running_turn_id
            .map(str::to_owned)
            .or_else(|| if same_thread { previous.and_then(|prev| prev.user_action_turn_id.clone()) } else { None }),
        user_action_revision: if !same_thread || new_turn {
            user_action_revision
        } else {
            previous.map_or(user_action_revision, |prev| prev.user_action_revision)
        },
    }
}

/// `shouldRetargetThreadPullRequestPanel` (`ChatView.logic.ts:188`).
pub fn should_retarget_thread_pull_request_panel(
    previous: Option<&PullRequestTarget>,
    current: Option<&PullRequestTarget>,
    open_surface: Option<&Surface>,
) -> bool {
    let (Some(previous), Some(current), Some(surface)) = (previous, current, open_surface) else {
        return false;
    };
    let SurfaceKind::PullRequest { project_id, repository, number, .. } = &surface.kind else {
        return false;
    };
    let prev_repo = previous.repository.to_ascii_lowercase();
    (previous.project_id != current.project_id
        || prev_repo != current.repository.to_ascii_lowercase()
        || previous.number != current.number)
        && *project_id == previous.project_id
        && repository.to_ascii_lowercase() == prev_repo
        && *number == previous.number
}

/// `shouldOpenProactiveTurnDiff` (`ChatView.logic.ts:205`).
pub fn should_open_proactive_turn_diff(
    previous_running_turn_id: Option<&Option<String>>,
    running_turn_id: Option<&str>,
    settled_turn_id: Option<&str>,
    turn_completed: bool,
) -> bool {
    running_turn_id.is_none()
        && turn_completed
        && settled_turn_id.is_some()
        && match previous_running_turn_id {
            None => true,
            Some(prev) => prev.as_deref() == settled_turn_id,
        }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProactiveTurnDiffAction {
    Defer,
    Ignore,
    Open,
}

/// `resolveProactiveTurnDiffAction` (`ChatView.logic.ts:220`): opens Changes for edits touching
/// at least 3 files or 50 changed lines (additions + deletions).
pub fn resolve_proactive_turn_diff_action(
    checkpoint: Option<&TurnDiffSummary>,
    is_git_repo: Option<bool>,
) -> ProactiveTurnDiffAction {
    let Some(checkpoint) = checkpoint else {
        return ProactiveTurnDiffAction::Defer;
    };
    if checkpoint.status == "missing" {
        return ProactiveTurnDiffAction::Defer;
    }
    let Some(is_git_repo) = is_git_repo else {
        return ProactiveTurnDiffAction::Defer;
    };
    if !is_git_repo || checkpoint.status != "ready" || checkpoint.files.is_empty() {
        return ProactiveTurnDiffAction::Ignore;
    }
    let totals = checkpoint.totals();
    if totals.count >= 3 || totals.additions + totals.deletions >= 50 {
        ProactiveTurnDiffAction::Open
    } else {
        ProactiveTurnDiffAction::Ignore
    }
}

/// Snapshot of thread/runtime/PR inputs consumed by `ProactivePanelCoordinator::evaluate`
/// (`apps/web/src/hooks/useProactivePanels.ts`).
pub struct ProactiveEvaluationInput<'a> {
    pub thread_id: Option<&'a str>,
    pub enabled: bool,
    pub settings_hydrated: bool,
    pub thread_detail_loading: bool,
    pub running_turn_id: Option<&'a str>,
    pub latest_run_id: Option<&'a str>,
    pub latest_run_completed: bool,
    pub latest_run_settled: bool,
    pub turn_diff_summaries: &'a [TurnDiffSummary],
    pub is_git_repo: Option<bool>,
    pub linked_pull_request: Option<PullRequestTarget>,
    pub has_linked_pull_request_detail: bool,
    pub pull_requests_key: Option<String>,
    pub pull_requests_capability_known: bool,
    pub supports_pull_requests: bool,
    pub pull_requests_surface_available: bool,
    pub visible_pull_request_count: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProactiveEvaluationOutcome {
    pub panels_changed: bool,
    pub diff_opened: bool,
}

/// Stateful coordinator porting `apps/web/src/hooks/useProactivePanels.ts`.
#[derive(Debug, Default)]
pub struct ProactivePanelCoordinator {
    observation: Option<ProactivePanelObservation>,
    observed_pull_requests: HashMap<String, Option<PullRequestTarget>>,
}

impl ProactivePanelCoordinator {
    pub fn evaluate(&mut self, panels: &mut RightPanels, input: ProactiveEvaluationInput<'_>) -> ProactiveEvaluationOutcome {
        let Some(thread_id) = input.thread_id else {
            self.observation = None;
            return ProactiveEvaluationOutcome::default();
        };
        let observation = observe_proactive_panel_user_choice(
            self.observation.as_ref(),
            thread_id,
            input.running_turn_id,
            panels.get_user_action_revision(thread_id),
        );
        let previous_running_turn_id = observation.running_turn_id.clone();
        let previous_target_key = observation.target_key.clone();
        let user_action_revision = observation.user_action_revision;
        self.observation = Some(observation);

        let open_surface = panels.active_surface(thread_id).cloned();
        let previous_pr = self.observed_pull_requests.get(thread_id).and_then(Clone::clone);
        if input.linked_pull_request.is_some() || !input.thread_detail_loading {
            self.observed_pull_requests.insert(thread_id.to_owned(), input.linked_pull_request.clone());
        }
        let follow_selected_pr = should_retarget_thread_pull_request_panel(
            previous_pr.as_ref(),
            input.linked_pull_request.as_ref(),
            open_surface.as_ref(),
        );
        let mut outcome = ProactiveEvaluationOutcome::default();
        if follow_selected_pr
            && let Some(pr) = input.linked_pull_request.clone()
            && panels.open_proactive(thread_id, ProactiveRequest::PullRequest(pr), user_action_revision, true)
        {
            outcome.panels_changed = true;
        }
        if !input.settings_hydrated {
            return outcome;
        }

        let eligible_link = input.enabled
            && should_open_proactive_pull_request(previous_target_key.as_ref(), input.pull_requests_key.as_deref());
        let should_defer_link = eligible_link && !input.pull_requests_capability_known;
        if let Some(obs) = &mut self.observation {
            obs.target_key = if should_defer_link {
                Some(previous_target_key.flatten())
            } else {
                Some(input.pull_requests_key.clone())
            };
        }
        if eligible_link && input.pull_requests_capability_known {
            if !follow_selected_pr
                && input.pull_requests_surface_available
                && (input.visible_pull_request_count > 1 || !input.has_linked_pull_request_detail || !input.supports_pull_requests)
            {
                if panels.open_proactive(thread_id, ProactiveRequest::PullRequests, user_action_revision, false) {
                    outcome.panels_changed = true;
                }
            } else if !follow_selected_pr
                && input.supports_pull_requests
                && let Some(pr) = input.linked_pull_request
                && panels.open_proactive(thread_id, ProactiveRequest::PullRequest(pr), user_action_revision, false)
            {
                outcome.panels_changed = true;
            }
        }
        if input.thread_detail_loading {
            return outcome;
        }

        let settled_turn_id = input.latest_run_settled.then_some(input.latest_run_id).flatten();
        let newly_completed_turn_id = should_open_proactive_turn_diff(
            previous_running_turn_id.as_ref(),
            input.running_turn_id,
            settled_turn_id,
            input.latest_run_completed,
        )
        .then_some(settled_turn_id)
        .flatten();
        let eligible_completion = input.enabled
            && newly_completed_turn_id.is_some()
            && !(input.pull_requests_key.is_some()
                && (!input.pull_requests_capability_known || input.supports_pull_requests || input.pull_requests_surface_available));
        let completed_checkpoint = eligible_completion
            .then(|| newly_completed_turn_id.and_then(|run_id| input.turn_diff_summaries.iter().find(|c| c.run_id == run_id)))
            .flatten();
        let diff_action = if eligible_completion {
            resolve_proactive_turn_diff_action(completed_checkpoint, input.is_git_repo)
        } else {
            ProactiveTurnDiffAction::Ignore
        };
        if let Some(obs) = &mut self.observation {
            obs.running_turn_id = if diff_action == ProactiveTurnDiffAction::Defer || should_defer_link {
                previous_running_turn_id
            } else {
                Some(input.running_turn_id.map(str::to_owned))
            };
        }
        if diff_action != ProactiveTurnDiffAction::Open {
            return outcome;
        }
        let Some(turn_id) = newly_completed_turn_id else {
            return outcome;
        };
        let before_offer = panels.panel(thread_id).proactive_diff_turn_id.clone();
        if panels.open_proactive(thread_id, ProactiveRequest::Diff { turn_id: turn_id.to_owned() }, user_action_revision, false) {
            outcome.panels_changed = true;
            outcome.diff_opened = true;
        } else if panels.panel(thread_id).proactive_diff_turn_id != before_offer {
            outcome.panels_changed = true;
        }
        outcome
    }
}

#[cfg(test)]
mod tests;
