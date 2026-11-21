//! Multi-route candidate selection, host classification, learned direct-endpoint merging, and
//! route fallback (`packages/shared/src/hostClassification.ts`,
//! `packages/client-runtime/src/connection/routes.ts`,
//! `packages/client-runtime/src/connection/driver.ts`).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

/// How long a route descriptor check waits before marking a route silent (`ROUTE_CHECK_TIMEOUT_MS`).
pub const ROUTE_CHECK_TIMEOUT: Duration = Duration::from_millis(2_500);
/// Head start shared by preferred direct routes before falling through (`ROUTE_CHECK_HEAD_START_MS`).
pub const ROUTE_CHECK_HEAD_START: Duration = Duration::from_millis(500);
/// Interval between periodic checks for a better route while connected (`BETTER_ROUTE_CHECK_INTERVAL_MS`).
pub const BETTER_ROUTE_CHECK_INTERVAL: Duration = Duration::from_millis(60_000);
/// Maximum backoff for a repeatedly silent better route (`SILENT_ROUTE_BACKOFF_MAX_MS`).
pub const SILENT_ROUTE_BACKOFF_MAX: Duration = Duration::from_millis(1_800_000);
/// Cooldown after a better route answered preflight but failed to connect (`BETTER_ROUTE_COOLDOWN_MS`).
pub const BETTER_ROUTE_COOLDOWN: Duration = Duration::from_millis(300_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionRouteKind {
    Loopback,
    Lan,
    Tailnet,
    Vpn,
    Public,
    Ssh,
    Relay,
}

impl ConnectionRouteKind {
    /// `ROUTE_KIND_RANK` in `packages/client-runtime/src/connection/routes.ts`.
    pub const fn rank(self) -> u8 {
        match self {
            ConnectionRouteKind::Loopback => 0,
            ConnectionRouteKind::Lan => 1,
            ConnectionRouteKind::Tailnet | ConnectionRouteKind::Vpn => 2,
            ConnectionRouteKind::Public => 3,
            ConnectionRouteKind::Ssh => 4,
            ConnectionRouteKind::Relay => 5,
        }
    }

    pub const fn is_nearby(self) -> bool {
        matches!(self, ConnectionRouteKind::Loopback | ConnectionRouteKind::Lan)
    }

    pub const fn label(self) -> &'static str {
        match self {
            ConnectionRouteKind::Relay => "T3 Connect",
            ConnectionRouteKind::Loopback => "This device",
            ConnectionRouteKind::Lan => "LAN",
            ConnectionRouteKind::Tailnet => "Tailscale",
            ConnectionRouteKind::Vpn => "VPN",
            ConnectionRouteKind::Ssh => "SSH",
            ConnectionRouteKind::Public => "Remote link",
        }
    }
}

/// `ReportedEndpoint` advertised in `ServerConfig.directEndpoints`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportedEndpoint {
    pub http_base_url: String,
    #[serde(default)]
    pub kind: Option<String>,
}

/// One candidate route to an environment (`ConnectionRoute`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateRoute {
    pub id: String,
    pub http_base: String,
    #[serde(default)]
    pub learned: bool,
    #[serde(default)]
    pub tailscale: bool,
    #[serde(default)]
    pub ssh: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay_access_token: Option<String>,
}

impl std::fmt::Debug for CandidateRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandidateRoute")
            .field("id", &self.id)
            .field("http_base", &self.http_base)
            .field("kind", &self.kind())
            .field("learned", &self.learned)
            .finish()
    }
}

impl CandidateRoute {
    pub fn primary(http_base: impl Into<String>, ssh: bool) -> Self {
        let http_base = normalize_http_base(&http_base.into());
        let id = if ssh { format!("ssh:{http_base}") } else { format!("direct:{http_base}") };
        Self { id, http_base, learned: false, tailscale: false, ssh, relay_access_token: None }
    }

    pub fn saved(http_base: impl Into<String>) -> Self {
        Self::primary(http_base, false)
    }

    pub fn relay(response: t3_core::relay::EnvironmentConnectResponse) -> Self {
        Self {
            id: format!("relay:{}", response.environment_id),
            http_base: normalize_http_base(&response.endpoint.http_base_url),
            learned: false,
            tailscale: false,
            ssh: false,
            relay_access_token: Some(response.credential),
        }
    }

    pub(crate) fn relay_access_token(&self) -> Option<&str> {
        self.relay_access_token.as_deref()
    }

    pub fn kind(&self) -> ConnectionRouteKind {
        if self.relay_access_token.is_some() {
            return ConnectionRouteKind::Relay;
        }
        if self.ssh {
            return ConnectionRouteKind::Ssh;
        }
        let Some(hostname) = route_hostname(&self.http_base) else {
            return ConnectionRouteKind::Public;
        };
        direct_address_kind(&hostname, self.tailscale)
    }

    /// `connectionRouteLabel` (`routes.ts`).
    pub fn label(&self) -> String {
        match self.kind() {
            ConnectionRouteKind::Public => route_hostname(&self.http_base).unwrap_or_else(|| "Remote link".to_owned()),
            other => other.label().to_owned(),
        }
    }
}

pub fn normalize_http_base(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_owned()
}

pub fn normalize_hostname(host: &str) -> String {
    host.trim()
        .to_ascii_lowercase()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_owned()
}

pub fn is_local_loopback_host(host: &str) -> bool {
    let normalized = normalize_hostname(host);
    if normalized == "localhost" || normalized == "::1" {
        return true;
    }
    normalized.parse::<Ipv4Addr>().is_ok_and(|ip| ip.octets()[0] == 127)
}

/// MagicDNS (`*.ts.net`) or Tailscale ULA `fd7a:115c:a1e0::/48` (`isTailnetHost`).
pub fn is_tailnet_host(host: &str) -> bool {
    let normalized = normalize_hostname(host);
    if normalized.ends_with(".ts.net") {
        return true;
    }
    normalized
        .parse::<Ipv6Addr>()
        .is_ok_and(|ip| matches!(ip.segments(), [0xfd7a, 0x115c, 0xa1e0, ..]))
}

/// CGNAT 100.64.0.0/10 (`isSharedAddressSpaceHost`).
pub fn is_shared_address_space_host(host: &str) -> bool {
    normalize_hostname(host)
        .parse::<Ipv4Addr>()
        .is_ok_and(|ip| {
            let [a, b, ..] = ip.octets();
            a == 100 && (64..=127).contains(&b)
        })
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 169 && b == 254)
        || (a == 198 && (18..=19).contains(&b))
}

pub fn is_private_network_host(host: &str) -> bool {
    let normalized = normalize_hostname(host);
    if normalized == "::"
        || is_local_loopback_host(&normalized)
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
        || normalized == "home.arpa"
        || normalized.ends_with(".home.arpa")
        || normalized.ends_with(".ts.net")
        || (!normalized.contains('.') && !normalized.contains(':'))
    {
        return true;
    }
    if let Ok(ip) = normalized.parse::<Ipv4Addr>() {
        return is_private_ipv4(ip);
    }
    if let Ok(ip) = normalized.parse::<Ipv6Addr>() {
        if let Some(mapped) = ip.to_ipv4_mapped() {
            return is_private_ipv4(mapped);
        }
        let first = ip.segments()[0];
        return (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80;
    }
    false
}

pub fn direct_address_kind(hostname: &str, tailscale: bool) -> ConnectionRouteKind {
    if is_local_loopback_host(hostname) {
        return ConnectionRouteKind::Loopback;
    }
    if tailscale || is_tailnet_host(hostname) {
        return ConnectionRouteKind::Tailnet;
    }
    if is_shared_address_space_host(hostname) {
        return ConnectionRouteKind::Vpn;
    }
    if is_private_network_host(hostname) {
        ConnectionRouteKind::Lan
    } else {
        ConnectionRouteKind::Public
    }
}

pub fn route_hostname(http_base: &str) -> Option<String> {
    Url::parse(http_base).ok()?.host_str().map(str::to_owned)
}

/// `routeKeepsItsHost`: TLS proves who answers, and loopback cannot move.
pub fn route_keeps_its_host(route: &CandidateRoute) -> bool {
    route.http_base.starts_with("https:") || route.kind() == ConnectionRouteKind::Loopback
}

/// `isNearbyAddress`: loopback or LAN answers quickly or is not there.
pub fn is_nearby_address(http_base: &str) -> bool {
    route_hostname(http_base).is_some_and(|host| direct_address_kind(&host, false).is_nearby())
}

/// `insertRoute`: places `route` by `ConnectionRouteKind::rank()`.
pub fn insert_route(
    routes: &[CandidateRoute],
    route: CandidateRoute,
    ahead_of_same_kind: bool,
) -> Vec<CandidateRoute> {
    let kind = route.kind();
    let rank = kind.rank();
    let index = routes.iter().position(|existing| {
        let existing_kind = existing.kind();
        existing_kind.rank() > rank || (ahead_of_same_kind && existing_kind == kind)
    });
    let mut out = routes.to_vec();
    match index {
        Some(pos) => out.insert(pos, route),
        None => out.push(route),
    }
    out
}

/// `upsertRoute`: updates an existing route by `id`, or inserts a new route ahead of its kind.
pub fn upsert_route(routes: &[CandidateRoute], route: CandidateRoute) -> Vec<CandidateRoute> {
    if routes.iter().any(|existing| existing.id == route.id) {
        routes
            .iter()
            .map(|existing| if existing.id == route.id { route.clone() } else { existing.clone() })
            .collect()
    } else {
        insert_route(routes, route, true)
    }
}

/// `tokenAllowsPlainHttp`: a paired token goes to plain HTTP outside the tailnet only when it was
/// paired over a plain HTTP LAN or public route.
fn token_allows_plain_http(routes: &[CandidateRoute], credential_route_id: &str) -> bool {
    routes.iter().find(|route| route.id == credential_route_id).is_some_and(|route| {
        route.http_base.starts_with("http:")
            && matches!(route.kind(), ConnectionRouteKind::Lan | ConnectionRouteKind::Public)
    })
}

/// `credentialConnectionId`: strips `learned:<env>:<origin>@<credentialId>` down to `<credentialId>`.
pub fn credential_connection_id(route_id: &str) -> &str {
    if let Some(rest) = route_id.strip_prefix("learned:")
        && let Some((_, credential_id)) = rest.rsplit_once('@')
    {
        return credential_id;
    }
    route_id
}

/// `sendsTokenInClear` (`routes.ts:361-377`): prevents sending a token paired over HTTPS/loopback/tailnet
/// over plain HTTP outside the tailnet.
pub fn sends_token_in_clear(
    routes: &[CandidateRoute],
    route: &CandidateRoute,
    tailscale_override: Option<bool>,
) -> bool {
    if route.ssh {
        return false;
    }
    let tailscale = tailscale_override.unwrap_or(route.tailscale);
    if tailscale || !route.http_base.starts_with("http:") {
        return false;
    }
    let credential_id = credential_connection_id(&route.id);
    credential_id != route.id && !token_allows_plain_http(routes, credential_id)
}

/// `usableRoutes`: keeps only routes that never leak a paired token in the clear.
pub fn usable_routes(routes: &[CandidateRoute]) -> Vec<CandidateRoute> {
    routes.iter().filter(|route| !sends_token_in_clear(routes, route, None)).cloned().collect()
}

/// `mergeLearnedRoutes` (`routes.ts:232-338`): reconciles `directEndpoints` reported by a server
/// into the environment's candidate routes, keeping non-learned routes and dropping stale/unsafe ones.
/// Returns `Some(updated)` only when the route list actually changed.
pub fn merge_learned_routes(
    environment_id: &str,
    saved: &[CandidateRoute],
    active_route_id: &str,
    reported: &[ReportedEndpoint],
    allow_insecure: bool,
) -> Option<Vec<CandidateRoute>> {
    let shared_credential = credential_connection_id(active_route_id);
    let mut reported_urls: HashMap<String, Url> = HashMap::new();
    let mut reported_order: Vec<String> = Vec::new();
    let mut tailscale_origins: HashSet<String> = HashSet::new();
    for endpoint in reported {
        let Ok(url) = Url::parse(endpoint.http_base_url.trim()) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https") {
            continue;
        }
        if url.scheme() == "http" && !allow_insecure {
            continue;
        }
        let Some(host) = url.host_str() else {
            continue;
        };
        if is_local_loopback_host(host) {
            continue;
        }
        let origin = normalize_http_base(&url.origin().ascii_serialization());
        if !reported_urls.contains_key(&origin) {
            reported_order.push(origin.clone());
        }
        reported_urls.insert(origin.clone(), url);
        if endpoint.kind.as_deref() == Some("tailnet") {
            tailscale_origins.insert(origin);
        }
    }

    let mut known: HashSet<String> = saved
        .iter()
        .filter(|route| !route.learned)
        .map(|route| normalize_http_base(&route.http_base))
        .collect();

    let mut next: Vec<CandidateRoute> = Vec::new();
    for route in saved {
        let norm = normalize_http_base(&route.http_base);
        if !route.learned {
            let mut kept = route.clone();
            if reported_urls.contains_key(&norm) {
                kept.tailscale = tailscale_origins.contains(&norm);
            }
            next.push(kept);
            continue;
        }
        if !reported_urls.contains_key(&norm) || known.contains(&norm) {
            continue;
        }
        let is_ts = tailscale_origins.contains(&norm);
        if sends_token_in_clear(saved, route, Some(is_ts)) {
            continue;
        }
        known.insert(norm);
        let mut kept = route.clone();
        kept.tailscale = is_ts;
        next.push(kept);
    }

    for origin in reported_order {
        if known.contains(&origin) {
            continue;
        }
        let Some(url) = reported_urls.get(&origin) else { continue };
        let is_ts = tailscale_origins.contains(&origin);
        if url.scheme() == "http" && !is_ts && !token_allows_plain_http(saved, shared_credential) {
            continue;
        }
        let route_id = format!("learned:{environment_id}:{origin}@{shared_credential}");
        let learned = CandidateRoute {
            id: route_id,
            http_base: origin,
            learned: true,
            tailscale: is_ts,
            ssh: false,
            relay_access_token: None,
        };
        next = insert_route(&next, learned, false);
    }

    let signature = |routes: &[CandidateRoute]| -> Vec<(String, String, ConnectionRouteKind)> {
        routes.iter().map(|r| (r.id.clone(), r.http_base.clone(), r.kind())).collect()
    };
    (signature(saved) != signature(&next)).then_some(next)
}

/// `routesAfterRemoving`: removing a paired route also drops learned routes borrowing its token.
pub fn routes_after_removing(routes: &[CandidateRoute], removed_id: &str) -> Vec<CandidateRoute> {
    if !routes.iter().any(|route| route.id == removed_id) {
        return routes.to_vec();
    }
    routes
        .iter()
        .filter(|route| {
            if route.id == removed_id {
                return false;
            }
            if !route.learned {
                return true;
            }
            credential_connection_id(&route.id) != removed_id
        })
        .cloned()
        .collect()
}

/// Result of checking whether a candidate route answers with the expected environment (`RouteCheck`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteCheck {
    Answered,
    Silent,
    Unchecked,
}

/// Result of preflighting a candidate route (`RoutePreflight`, `pingdotgg/t3code#33199`):
/// `Unprepared` means the route answered its check but could not be prepared;
/// `Refused` means its host refused this device's credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutePreflight {
    Ready,
    Refused,
    Unprepared,
    Silent,
    Unchecked,
}

/// Outcome of trying to connect over one route.
pub enum RouteAttemptError<E> {
    /// Server protocol incompatibility — same server on every route, stop immediately.
    Unsupported(E),
    /// Host answered and refused this device's credential (`isHostCredentialRefusal`, `pingdotgg/t3code#33199`).
    /// Recorded in `refused_route_ids` if another route connects.
    Refused(E),
    /// Non-transient block (unauthorized / blocked) — try other routes unless none succeed.
    Blocked(E),
    /// Transient failure — try remaining routes and retain first transient failure if all fail.
    Transient(E),
}

/// Connected route plus any earlier routes whose host refused this device's sign-in (`pingdotgg/t3code#33199`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteConnectResult<T> {
    pub route: CandidateRoute,
    pub value: T,
    pub refused_route_ids: Vec<String>,
}

/// `mergeRefusedRoutes` (`packages/client-runtime/src/connection/supervisor.ts`, `pingdotgg/t3code#33199`):
/// merges newly refused route IDs while keeping only those that satisfy `keep` and deduplicating.
pub fn merge_refused_routes(
    existing: &[String],
    newly_refused: &[String],
    keep: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    for id in existing.iter().chain(newly_refused.iter()) {
        if keep(id) && !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

/// `connectOverRoutes` (`packages/client-runtime/src/connection/driver.ts:71-141`):
/// Probes candidate routes in parallel when more than one usable route exists, gives preferred
/// routes a `head_start` before falling through to slower routes, stops immediately on protocol
/// mismatch, tracks routes whose host refused this device's sign-in (`pingdotgg/t3code#33199`), and falls back
/// to late-answering routes before giving up.
pub async fn connect_over_routes<T, E, CheckFn, CheckFut, ConnFn, ConnFut>(
    routes: &[CandidateRoute],
    head_start: Duration,
    check_route: CheckFn,
    mut connect_route: ConnFn,
) -> Result<RouteConnectResult<T>, Option<E>>
where
    CheckFn: Fn(CandidateRoute) -> CheckFut,
    CheckFut: Future<Output = RouteCheck> + Send + 'static,
    ConnFn: FnMut(CandidateRoute) -> ConnFut,
    ConnFut: Future<Output = Result<T, RouteAttemptError<E>>>,
{
    let candidates = usable_routes(routes);
    if candidates.is_empty() {
        return Err(None);
    }
    if candidates.len() == 1 {
        let route = candidates[0].clone();
        return match connect_route(route.clone()).await {
            Ok(value) => Ok(RouteConnectResult {
                route,
                value,
                refused_route_ids: Vec::new(),
            }),
            Err(
                RouteAttemptError::Unsupported(err)
                | RouteAttemptError::Refused(err)
                | RouteAttemptError::Blocked(err)
                | RouteAttemptError::Transient(err),
            ) => Err(Some(err)),
        };
    }

    let mut checks: Vec<Option<tokio::task::JoinHandle<RouteCheck>>> = candidates
        .iter()
        .cloned()
        .map(|route| Some(tokio::spawn(check_route(route))))
        .collect();
    let head_start_deadline = tokio::time::Instant::now() + head_start;

    let mut transient: Option<E> = None;
    let mut blocked: Option<E> = None;
    let mut refused_route_ids: Vec<String> = Vec::new();
    let mut silent_indices: Vec<usize> = Vec::new();

    for (index, route) in candidates.iter().enumerate() {
        let remaining = head_start_deadline.saturating_duration_since(tokio::time::Instant::now());
        let check = if remaining.is_zero() {
            let handle = checks[index].as_mut().expect("check handle");
            if handle.is_finished() {
                checks[index].take().unwrap().await.unwrap_or(RouteCheck::Silent)
            } else {
                RouteCheck::Silent
            }
        } else {
            let handle = checks[index].as_mut().expect("check handle");
            match tokio::time::timeout(remaining, handle).await {
                Ok(joined) => {
                    checks[index] = None;
                    joined.unwrap_or(RouteCheck::Silent)
                }
                Err(_) => RouteCheck::Silent,
            }
        };

        if check == RouteCheck::Silent {
            silent_indices.push(index);
            continue;
        }
        match connect_route(route.clone()).await {
            Ok(value) => {
                abort_remaining(&mut checks);
                return Ok(RouteConnectResult {
                    route: route.clone(),
                    value,
                    refused_route_ids,
                });
            }
            Err(RouteAttemptError::Unsupported(err)) => {
                abort_remaining(&mut checks);
                return Err(Some(err));
            }
            Err(RouteAttemptError::Transient(err)) => {
                if transient.is_none() {
                    transient = Some(err);
                }
            }
            Err(RouteAttemptError::Refused(err)) => {
                if !refused_route_ids.contains(&route.id) {
                    refused_route_ids.push(route.id.clone());
                }
                if blocked.is_none() {
                    blocked = Some(err);
                }
            }
            Err(RouteAttemptError::Blocked(err)) => {
                if blocked.is_none() {
                    blocked = Some(err);
                }
            }
        }
    }

    let mut answered_late = Vec::new();
    let mut still_silent = Vec::new();
    for index in silent_indices {
        let check = match checks[index].take() {
            Some(handle) => handle.await.unwrap_or(RouteCheck::Silent),
            None => RouteCheck::Silent,
        };
        if check != RouteCheck::Silent {
            answered_late.push(index);
        } else {
            still_silent.push(index);
        }
    }

    for &index in &answered_late {
        let route = &candidates[index];
        match connect_route(route.clone()).await {
            Ok(value) => {
                return Ok(RouteConnectResult {
                    route: route.clone(),
                    value,
                    refused_route_ids,
                });
            }
            Err(RouteAttemptError::Unsupported(err)) => return Err(Some(err)),
            Err(RouteAttemptError::Transient(err)) => {
                if transient.is_none() {
                    transient = Some(err);
                }
            }
            Err(RouteAttemptError::Refused(err)) => {
                if !refused_route_ids.contains(&route.id) {
                    refused_route_ids.push(route.id.clone());
                }
                if blocked.is_none() {
                    blocked = Some(err);
                }
            }
            Err(RouteAttemptError::Blocked(err)) => {
                if blocked.is_none() {
                    blocked = Some(err);
                }
            }
        }
    }

    // Only attempt still-silent routes when no earlier route already produced a retryable transient error.
    if transient.is_none() {
        for &index in &still_silent {
            let route = &candidates[index];
            match connect_route(route.clone()).await {
                Ok(value) => {
                    return Ok(RouteConnectResult {
                        route: route.clone(),
                        value,
                        refused_route_ids,
                    });
                }
                Err(RouteAttemptError::Unsupported(err)) => return Err(Some(err)),
                Err(RouteAttemptError::Transient(err)) => {
                    if transient.is_none() {
                        transient = Some(err);
                    }
                    break;
                }
                Err(RouteAttemptError::Refused(err)) => {
                    if !refused_route_ids.contains(&route.id) {
                        refused_route_ids.push(route.id.clone());
                    }
                    if blocked.is_none() {
                        blocked = Some(err);
                    }
                }
                Err(RouteAttemptError::Blocked(err)) => {
                    if blocked.is_none() {
                        blocked = Some(err);
                    }
                }
            }
        }
    }

    Err(transient.or(blocked))
}

fn abort_remaining(checks: &mut [Option<tokio::task::JoinHandle<RouteCheck>>]) {
    for handle in checks.iter_mut().flatten() {
        handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_relay_candidate_preserves_its_credentials_and_falls_back_after_direct_and_ssh() {
        let direct = CandidateRoute::saved("https://direct.example.test");
        let ssh = CandidateRoute::primary("http://127.0.0.1:4411", true);
        let saved = serde_json::json!({
            "id": "relay:env-1",
            "httpBase": "https://relay.example.test",
            "relayAccessToken": "relay-only-token"
        });
        let relay: CandidateRoute = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(relay.kind(), ConnectionRouteKind::Relay);
        let routes = upsert_route(&[direct.clone(), ssh.clone()], relay.clone());
        assert_eq!(routes, [direct.clone(), ssh, relay.clone()]);
        let encoded = serde_json::to_value(relay).unwrap();
        assert_eq!(encoded["relayAccessToken"], saved["relayAccessToken"]);
        assert!(serde_json::to_value(direct).unwrap().get("relayAccessToken").is_none());
    }

    #[test]
    fn host_classification_and_route_ranking_match_web() {
        assert_eq!(direct_address_kind("127.0.0.1", false), ConnectionRouteKind::Loopback);
        assert_eq!(direct_address_kind("localhost", false), ConnectionRouteKind::Loopback);
        assert_eq!(direct_address_kind("192.168.1.42", false), ConnectionRouteKind::Lan);
        assert_eq!(direct_address_kind("studio.ts.net", false), ConnectionRouteKind::Tailnet);
        assert_eq!(direct_address_kind("100.64.1.2", false), ConnectionRouteKind::Vpn);
        assert_eq!(direct_address_kind("100.64.1.2", true), ConnectionRouteKind::Tailnet);
        assert_eq!(direct_address_kind("t3.example.com", false), ConnectionRouteKind::Public);

        assert!(is_nearby_address("http://192.168.1.42:3773"));
        assert!(is_nearby_address("http://127.0.0.1:3773"));
        assert!(!is_nearby_address("https://studio.ts.net:3773"));

        let public = CandidateRoute::saved("https://box.example.com:3773");
        let tailnet = CandidateRoute::saved("https://box.ts.net:3773");
        let lan = CandidateRoute::saved("http://192.168.1.5:3773");
        let routes = insert_route(&insert_route(&[public], tailnet, false), lan, false);
        assert_eq!(
            routes.iter().map(CandidateRoute::kind).collect::<Vec<_>>(),
            vec![ConnectionRouteKind::Lan, ConnectionRouteKind::Tailnet, ConnectionRouteKind::Public]
        );
    }

    #[test]
    fn merge_learned_routes_enforces_clear_token_rule_and_drops_loopback() {
        let https_primary = CandidateRoute::primary("https://studio.ts.net:3773", false);
        let reported = vec![
            ReportedEndpoint { http_base_url: "http://127.0.0.1:3773".into(), kind: None },
            ReportedEndpoint { http_base_url: "http://192.168.1.20:3773".into(), kind: Some("lan".into()) },
            ReportedEndpoint { http_base_url: "http://100.80.1.2:3773".into(), kind: Some("tailnet".into()) },
            ReportedEndpoint { http_base_url: "https://studio-alt.ts.net:3773".into(), kind: Some("tailnet".into()) },
        ];
        let merged = merge_learned_routes("env-1", std::slice::from_ref(&https_primary), &https_primary.id, &reported, true)
            .expect("learned routes added");
        // Loopback is excluded; plain HTTP LAN is excluded because token was paired over HTTPS;
        // tailnet HTTP and HTTPS are allowed.
        let bases: Vec<&str> = merged.iter().map(|r| r.http_base.as_str()).collect();
        assert!(!bases.contains(&"http://127.0.0.1:3773"));
        assert!(!bases.contains(&"http://192.168.1.20:3773"));
        assert!(bases.contains(&"http://100.80.1.2:3773"));
        assert!(bases.contains(&"https://studio-alt.ts.net:3773"));
        assert_eq!(usable_routes(&merged).len(), merged.len());

        // Removing the primary route also removes learned routes borrowing its token.
        assert!(routes_after_removing(&merged, &https_primary.id).is_empty());
    }

    #[tokio::test]
    async fn connect_over_routes_falls_back_from_silent_primary_to_answered_alternate() {
        let dead = CandidateRoute::saved("http://192.168.1.99:3773");
        let live = CandidateRoute::saved("https://studio.ts.net:3773");
        let routes = vec![dead.clone(), live.clone()];
        let mut tried = Vec::new();
        let result = connect_over_routes(
            &routes,
            Duration::from_millis(40),
            |route| async move {
                if route.http_base.contains("192.168.1.99") {
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    RouteCheck::Silent
                } else {
                    RouteCheck::Answered
                }
            },
            |route| {
                tried.push(route.http_base.clone());
                async move {
                    if route.http_base.contains("studio.ts.net") {
                        Ok::<_, RouteAttemptError<&'static str>>("connected-tailnet")
                    } else {
                        Err(RouteAttemptError::Transient("unreachable"))
                    }
                }
            },
        )
        .await
        .expect("fallback route succeeds");
        assert_eq!(result.route.http_base, live.http_base);
        assert_eq!(result.value, "connected-tailnet");
        assert!(result.refused_route_ids.is_empty());
        assert_eq!(tried, vec!["https://studio.ts.net:3773"]);
    }

    #[tokio::test]
    async fn connect_over_routes_records_refused_routes_when_fallback_succeeds() {
        let refused = CandidateRoute::saved("http://192.168.1.10:3773");
        let live = CandidateRoute::saved("https://studio.ts.net:3773");
        let routes = vec![refused.clone(), live.clone()];
        let result = connect_over_routes(
            &routes,
            Duration::from_millis(50),
            |_| async { RouteCheck::Answered },
            |route| async move {
                if route.http_base.contains("192.168.1.10") {
                    Err(RouteAttemptError::Refused("sign-in refused"))
                } else {
                    Ok::<_, RouteAttemptError<&'static str>>("connected-tailnet")
                }
            },
        )
        .await
        .expect("fallback connects after refused route");
        assert_eq!(result.route.id, live.id);
        assert_eq!(result.refused_route_ids, vec![refused.id.clone()]);

        let merged = merge_refused_routes(
            std::slice::from_ref(&refused.id),
            &[live.id.clone(), refused.id.clone()],
            |id| id != live.id,
        );
        assert_eq!(merged, vec![refused.id]);
    }

    #[tokio::test]
    async fn connect_over_routes_stops_immediately_on_unsupported_protocol() {
        let first = CandidateRoute::saved("http://192.168.1.5:3773");
        let second = CandidateRoute::saved("https://studio.ts.net:3773");
        let mut tried = Vec::new();
        let result = connect_over_routes(
            &[first, second],
            Duration::from_millis(50),
            |_| async { RouteCheck::Answered },
            |route| {
                tried.push(route.http_base.clone());
                async { Err::<(), _>(RouteAttemptError::Unsupported("server too old")) }
            },
        )
        .await;
        assert_eq!(result.err(), Some(Some("server too old")));
        assert_eq!(tried, vec!["http://192.168.1.5:3773"]);
    }
}
