//! What a paired device may do: `AuthSessionState` from `GET /api/auth/session` and
//! `sessionGrantsScope`, both in `packages/contracts/src/auth.ts`. Scopes stay strings so a
//! newer server's scopes decode and grant nothing.

use serde::Deserialize;

pub const TERMINAL_READ_SCOPE: &str = "terminal:read";
pub const TERMINAL_OPERATE_SCOPE: &str = "terminal:operate";
pub const SOURCE_CONTROL_WRITE_SCOPE: &str = "source-control:write";
pub const ORCHESTRATION_OPERATE_SCOPE: &str = "orchestration:operate";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionState {
    pub authenticated: bool,
    #[serde(default)]
    pub auth: AuthDescriptor,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default)]
    pub permissions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthDescriptor {
    #[serde(default)]
    pub server_update_scope: Option<String>,
}

/// `legacyParents`: older credentials carry the parent scope instead of the split ones.
const LEGACY_PARENTS: &[(&str, &str)] = &[
    ("filesystem:read", "orchestration:read"),
    ("diagnostics:read", "orchestration:read"),
    ("settings:write", "orchestration:operate"),
    ("providers:manage", "orchestration:operate"),
    ("environment:maintain", "orchestration:operate"),
    ("preview:operate", "orchestration:operate"),
    ("source-control:write", "orchestration:operate"),
    ("filesystem:write", "orchestration:operate"),
    (TERMINAL_READ_SCOPE, TERMINAL_OPERATE_SCOPE),
];

impl AuthSessionState {
    /// `sessionGrantsScope`.
    pub fn grants(&self, scope: &str) -> bool {
        if !self.authenticated {
            return false;
        }
        if let Some(permissions) = &self.permissions {
            return permissions.iter().any(|granted| granted == scope);
        }
        let has = |scope: &str| self.scopes.as_ref().is_some_and(|scopes| scopes.iter().any(|granted| granted == scope));
        if has(scope) {
            return true;
        }
        if self.auth.server_update_scope.is_some() {
            return false;
        }
        LEGACY_PARENTS.iter().find(|(child, _)| *child == scope).is_some_and(|(_, parent)| has(parent))
    }
}
