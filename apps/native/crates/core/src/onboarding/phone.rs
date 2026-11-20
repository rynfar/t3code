//! Phone section visibility and T3 Connect link-state derivation for the Welcome page
//! (`PhoneSection` + `PhoneConnectLink` in `apps/web/src/components/onboarding/WelcomePage.tsx`,
//! `welcomeOffersPhone` in `apps/web/src/onboarding/welcomePage.logic.ts`, and `MOBILE_APP_URL` in
//! `apps/web/src/branding.ts`).

use serde_json::Value;

use crate::managed_policy::{Capability, capability_denial};
use crate::mcp::gate::{CloudLinkState, ConnectReachability};
use crate::relay::Discovery;

/// `MOBILE_APP_URL` (`apps/web/src/branding.ts`): redirects to the App Store or Google Play to
/// match the phone that scans it.
pub const MOBILE_APP_URL: &str = "https://t3.codes/mobile";

/// How often `PhoneConnectLink` refreshes `GET /api/connect/link-state` while expanded
/// (`WelcomePage.tsx` `30_000` ms interval).
pub const PHONE_LINK_REFRESH_INTERVAL_SECS: u64 = 30;

/// Inputs to `welcome_offers_phone` (`welcomeOffersPhone` in `welcomePage.logic.ts`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WelcomePhoneOfferInput<'a> {
    pub is_primary: bool,
    pub cloud_configured: bool,
    /// Both `relay:read` and `relay:write` scopes granted on this environment.
    pub can_share: bool,
    pub managed_policy: Option<&'a Value>,
}

/// `welcomeOffersPhone` (`apps/web/src/onboarding/welcomePage.logic.ts`):
/// The phone joins by signing in to T3 Connect, which shares the primary computer. Another
/// computer, a build without T3 Connect, missing relay scopes, or a policy that turns remote
/// clients or T3 Connect off gets no Phone section.
pub fn welcome_offers_phone(input: WelcomePhoneOfferInput<'_>) -> bool {
    input.is_primary
        && input.cloud_configured
        && input.can_share
        && capability_denial(input.managed_policy, Capability::RemoteClients).is_none()
        && capability_denial(input.managed_policy, Capability::Connect).is_none()
}

/// How the signed-in T3 Connect account is compared against `CloudLinkState.cloud_user_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneCloudAccount<'a> {
    /// Not signed in to T3 Connect.
    SignedOut,
    /// Signed in with a known Clerk user ID (`sub`), compared directly against `cloudUserId`.
    UserId(&'a str),
    /// Signed in via native OAuth where `CloudState.identity` holds a display email/username
    /// rather than Clerk's `sub`, optionally cross-checked against the account's listed relay
    /// environments (`Some(true)` = present, `Some(false)` = listed and absent, `None` = not
    /// yet listed).
    NativeIdentity { in_account_relay_list: Option<bool> },
}

/// Derives `PhoneCloudAccount` from native `CloudState.identity` and relay `Discovery`.
pub fn phone_account_for_identity<'a>(
    identity: Option<&'a str>,
    discovery: &Discovery,
    environment_ids: &[&str],
) -> PhoneCloudAccount<'a> {
    let Some(identity) = identity.map(str::trim).filter(|id| !id.is_empty()) else {
        return PhoneCloudAccount::SignedOut;
    };
    if identity.starts_with("user_") {
        return PhoneCloudAccount::UserId(identity);
    }
    let in_account_relay_list = discovery.listed.then(|| {
        discovery.environments.iter().any(|env| {
            environment_ids
                .iter()
                .any(|id| !id.is_empty() && *id == env.environment_id)
        })
    });
    PhoneCloudAccount::NativeIdentity { in_account_relay_list }
}

/// Whether the computer is actively shared over T3 Connect for the signed-in account
/// (`shared` in `WelcomePage.tsx` `PhoneConnectLink`).
///
/// Older servers omit `managedTunnelActive` (`None`), which counts as active while `linked` is
/// true and `linkRevoked` is not true.
pub fn phone_is_shared(
    link: Option<&CloudLinkState>,
    account: PhoneCloudAccount<'_>,
) -> bool {
    let Some(link) = link else {
        return false;
    };
    if !link.linked || link.link_revoked == Some(true) || link.managed_tunnel_active == Some(false) {
        return false;
    }
    match account {
        PhoneCloudAccount::SignedOut => false,
        PhoneCloudAccount::UserId(user_id) => link.cloud_user_id.as_deref() == Some(user_id),
        PhoneCloudAccount::NativeIdentity { in_account_relay_list } => match in_account_relay_list {
            Some(matched) => matched,
            None => link.cloud_user_id.as_deref().is_some_and(|id| !id.is_empty()),
        },
    }
}

/// Inputs to `resolve_phone_connect_state` (`PhoneConnectLink` in `WelcomePage.tsx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhoneConnectInput<'a> {
    pub account: PhoneCloudAccount<'a>,
    pub link: Option<&'a CloudLinkState>,
    pub is_pending: bool,
    pub has_error: bool,
}

/// All display states of `PhoneConnectLink` (`WelcomePage.tsx:1065-1125`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneConnectState {
    /// 1. Signed in, `link == None`, and fetch is pending: `"Checking T3 Connect…"`.
    Checking,
    /// 2. Signed in and fetch failed: `"Could not check T3 Connect."` + `"Check T3 Connect"`.
    Error,
    /// 3. Signed in, shared, and `reachability == "reachable"`: checkmark + reachable copy.
    Reachable,
    /// 4. Signed in, shared, and `reachability == None`: T3 Connect is on copy.
    SharedUnknownReachability,
    /// 5a. Signed in, shared, and `reachability == "unreachable"`: unreachable copy + `"Check T3 Connect"`.
    SharedUnreachable,
    /// 5b. Signed in, not shared: share this computer copy + `"Set up T3 Connect"`.
    NotShared,
    /// 6. Signed out: sign-in copy + `"Sign in to T3 Connect"`.
    SignedOut,
}

/// Derives the `PhoneConnectLink` state (`WelcomePage.tsx:1065-1125`).
pub fn resolve_phone_connect_state(input: &PhoneConnectInput<'_>) -> PhoneConnectState {
    if matches!(input.account, PhoneCloudAccount::SignedOut) {
        return PhoneConnectState::SignedOut;
    }
    if input.link.is_none() && input.is_pending {
        return PhoneConnectState::Checking;
    }
    if input.has_error {
        return PhoneConnectState::Error;
    }
    if !phone_is_shared(input.link, input.account) {
        return PhoneConnectState::NotShared;
    }
    match input.link.and_then(|link| link.reachability.as_ref()) {
        Some(ConnectReachability::Reachable { .. }) => PhoneConnectState::Reachable,
        Some(ConnectReachability::Unreachable { .. }) => PhoneConnectState::SharedUnreachable,
        None => PhoneConnectState::SharedUnknownReachability,
    }
}
