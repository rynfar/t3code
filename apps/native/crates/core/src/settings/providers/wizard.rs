//! Ports `AddProviderInstanceDialog.tsx` and `.logic.ts`; field metadata lives in `form`.
use super::{form, new_instance};
use crate::server_config::ProviderInstanceConfig;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
pub struct ConfigDraft {
    pub value: Option<Value>,
    errors: HashMap<String, String>,
}
impl ConfigDraft {
    pub fn from_value(value: Value) -> Self {
        Self {
            value: Some(value),
            ..Default::default()
        }
    }
    pub fn edit(&mut self, driver: &str, key: &str, value: Value) {
        if !form::validate_field(driver, key, &value) {
            self.errors.insert(
                key.into(),
                "Compact after 100,000 to 1,000,000 tokens. Leave empty to use Claude's default."
                    .into(),
            );
            return;
        }
        self.errors.remove(key);
        let instance = ProviderInstanceConfig {
            driver: driver.into(),
            config: self.value.take(),
            ..Default::default()
        };
        self.value = form::with_field(&instance, key, value).config;
    }
    pub fn error(&self) -> Option<&str> {
        self.errors.values().next().map(String::as_str)
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpSetup {
    #[default]
    Choices,
    Manual,
}
impl AcpSetup {
    pub fn start(local: bool) -> (Self, ConfigDraft, &'static str) {
        (
            Self::Manual,
            ConfigDraft {
                value: local.then(|| serde_json::json!({"source":"local","commandArgs":[]})),
                ..Default::default()
            },
            if local { "Local ACP" } else { "" },
        )
    }
    pub fn local(self, config: Option<&Value>) -> bool {
        self == Self::Manual && config.is_some_and(|c| c["source"] == "local")
    }
    pub fn selection_error(self, config: Option<&Value>) -> Option<&'static str> {
        let local = self.local(config);
        let key = if local { "commandPath" } else { "agentId" };
        let filled = config
            .and_then(|c| c[key].as_str())
            .is_some_and(|s| !s.trim().is_empty());
        if self == Self::Manual && filled {
            None
        } else if local {
            Some("Executable is required.")
        } else {
            Some("Select an ACP or configure one manually.")
        }
    }
    pub fn steps(self, driver: &str, config: Option<&Value>) -> &'static [&'static str] {
        if driver != super::ACP_REGISTRY {
            &STEPS
        } else if self.local(config) {
            &["Provider", "Identity"]
        } else {
            &["Provider", "Identity", "Sign in"]
        }
    }
    pub fn navigate(
        self,
        driver: &str,
        config: Option<&Value>,
        current: usize,
        requested: usize,
        identity_error: Option<String>,
    ) -> Result<usize, (usize, String)> {
        let target = requested.min(self.steps(driver, config).len() - 1);
        if driver == super::ACP_REGISTRY
            && current == 0
            && target > 0
            && let Some(error) = self.selection_error(config)
        {
            return Err((0, error.into()));
        }
        navigation(current, target, identity_error)
    }
}

pub const STEPS: [&str; 3] = ["Provider", "Identity", "Config"];

pub fn navigation(
    current: usize,
    requested: usize,
    identity_error: Option<String>,
) -> Result<usize, (usize, String)> {
    let target = requested.min(STEPS.len() - 1);
    if current <= 1
        && target > 1
        && let Some(error) = identity_error
    {
        return Err((1, error));
    }
    Ok(target)
}

#[derive(Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Dialog,
    Identity,
    Config(String),
}

pub fn focus_target(
    step: usize,
    driver: &str,
    config: Option<&Value>,
    policy: Option<&Value>,
    can_manage: bool,
) -> FocusTarget {
    if !can_manage || !crate::managed_policy::driver_allowed(policy, driver) {
        return FocusTarget::Dialog;
    }
    if step == 1 {
        return FocusTarget::Identity;
    }
    if step == 2 && can_manage && crate::managed_policy::driver_allowed(policy, driver) {
        return form::fields(driver, config)
            .into_iter()
            .find(|field| {
                !crate::managed_policy::instance_field_locked(
                    policy,
                    driver,
                    &format!("config.{}", field.key),
                )
            })
            .map_or(FocusTarget::Dialog, |field| FocusTarget::Config(field.key));
    }
    FocusTarget::Dialog
}

pub fn instance(
    driver: &str,
    label: &str,
    accent: &str,
    config: Option<Value>,
    local_environment: Vec<Value>,
) -> ProviderInstanceConfig {
    let mut config = config
        .and_then(|c| c.as_object().cloned())
        .unwrap_or_default();
    if driver == "codex" {
        config.insert("setupMode".into(), Value::String("existing".into()));
    }
    let local = driver == super::ACP_REGISTRY && config.get("source").is_some_and(|s| s == "local");
    ProviderInstanceConfig {
        config: (!config.is_empty()).then_some(Value::Object(config)),
        environment: (local && !local_environment.is_empty())
            .then_some(Value::Array(local_environment)),
        ..new_instance(driver, label, accent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::providers::create_payload;
    use serde_json::json;

    #[test]
    fn manual_and_local_paths_reset_only_the_web_owned_drafts_and_gate_navigation() {
        let (mode, mut config, label) = AcpSetup::start(false);
        assert_eq!(label, "");
        assert!(config.value.is_none());
        assert_eq!(
            mode.navigate("acpRegistry", None, 0, 2, None),
            Err((0, "Select an ACP or configure one manually.".into()))
        );
        config.edit("acpRegistry", "agentId", json!("  devin  "));
        config.edit("acpRegistry", "commandPath", json!("/override"));
        config.edit("acpRegistry", "authMethodId", json!("token"));
        assert_eq!(mode.selection_error(config.value.as_ref()), None);
        let manual = instance(
            "acpRegistry",
            " Registry Agent ",
            "",
            config.value.clone(),
            vec![json!({"name":"IGNORED","value":"local only"})],
        );
        assert_eq!(
            create_payload("registry_agent", &manual)["providerInstanceMutation"]["instance"],
            json!({"driver":"acpRegistry","enabled":true,"displayName":"Registry Agent","config":{"agentId":"  devin  ","commandPath":"/override","authMethodId":"token"}})
        );
        assert_eq!(
            mode.navigate(
                "acpRegistry",
                config.value.as_ref(),
                0,
                2,
                Some("duplicate".into())
            ),
            Err((1, "duplicate".into()))
        );
        assert!(
            AcpSetup::Choices
                .selection_error(config.value.as_ref())
                .is_some()
        );
        let (mode, mut local, label) = AcpSetup::start(true);
        assert_eq!(label, "Local ACP");
        assert_eq!(
            local.value,
            Some(json!({"source":"local","commandArgs":[]}))
        );
        assert_eq!(
            mode.selection_error(local.value.as_ref()),
            Some("Executable is required.")
        );
        local.edit("acpRegistry", "commandPath", json!("  dsh  "));
        assert_eq!(
            mode.steps("acpRegistry", local.value.as_ref()),
            ["Provider", "Identity"]
        );
        assert_eq!(
            mode.navigate(
                "acpRegistry",
                local.value.as_ref(),
                0,
                99,
                Some("duplicate".into())
            ),
            Ok(1)
        );
        local.edit("acpRegistry", "source", json!(""));
        assert_eq!(
            mode.selection_error(local.value.as_ref()),
            Some("Select an ACP or configure one manually.")
        );
        assert_eq!(
            mode.navigate(
                "acpRegistry",
                local.value.as_ref(),
                1,
                0,
                Some("duplicate".into())
            ),
            Ok(0)
        );
    }

    #[test]
    fn draft_validation_keeps_the_last_valid_value_and_errors_until_corrected() {
        let mut draft = ConfigDraft {
            value: Some(json!({"future":true})),
            ..Default::default()
        };
        draft.edit("claudeAgent", "autoCompactWindow", json!("300000"));
        draft.edit("claudeAgent", "autoCompactWindow", json!("99999"));
        assert!(draft.error().is_some());
        draft.edit("claudeAgent", "binaryPath", json!("custom"));
        assert!(draft.error().is_some());
        assert_eq!(draft.value.as_ref().unwrap()["autoCompactWindow"], "300000");
        draft.edit("claudeAgent", "autoCompactWindow", json!(""));
        assert!(draft.error().is_none());
        assert_eq!(
            draft.value,
            Some(json!({"future":true,"binaryPath":"custom"}))
        );
    }

    #[test]
    fn identity_blocks_forward_step_jumps_but_allows_back_and_identity() {
        for (current, requested, expected) in
            [(0, 2, 1), (1, 2, 1), (2, 0, 0), (2, 1, 1), (0, 1, 1)]
        {
            let result = navigation(current, requested, Some("duplicate".into()));
            assert_eq!(
                result.as_ref().copied().unwrap_or_else(|(step, _)| *step),
                expected
            );
            assert_eq!(result.is_err(), current <= 1 && requested > 1);
        }
        assert_eq!(navigation(0, 99, None), Ok(2));
    }

    #[test]
    fn transitions_keep_focus_inside_the_dialog_and_skip_locked_fields_in_schema_order() {
        let locked_binary = json!({"limits":{"providers":{"lockedInstanceFields":{"codex":["config.binaryPath"]}}}});
        let locked_runtime =
            json!({"limits":{"providers":{"lockedInstanceFields":{"codex":["config"]}}}});
        for (driver, policy, can_manage, expected) in [
            (
                "codex",
                None,
                true,
                FocusTarget::Config("binaryPath".into()),
            ),
            (
                "antigravity",
                None,
                true,
                FocusTarget::Config("authMethod".into()),
            ),
            (
                "codex",
                Some(&locked_binary),
                true,
                FocusTarget::Config("homePath".into()),
            ),
            ("codex", Some(&locked_runtime), true, FocusTarget::Dialog),
            ("cursor", None, true, FocusTarget::Dialog),
            ("kiro", None, false, FocusTarget::Dialog),
        ] {
            let step = navigation(1, 2, None).unwrap();
            assert_eq!(
                focus_target(step, driver, None, policy, can_manage),
                expected
            );
        }
        assert_eq!(
            focus_target(navigation(2, 0, None).unwrap(), "codex", None, None, true),
            FocusTarget::Dialog
        );
        assert_eq!(
            focus_target(1, "codex", None, None, false),
            FocusTarget::Dialog
        );
        assert_eq!(
            focus_target(navigation(2, 1, None).unwrap(), "codex", None, None, true),
            FocusTarget::Identity
        );
    }

    #[test]
    fn create_keeps_schema_edits_secrets_and_unknown_config_without_defaults() {
        let config = json!({"binaryPath":"custom", "launchArgs":"--verbose", "future":{"key":1}, "setupMode":"managed"});
        let codex = instance("codex", " Side ", "#ABCDEF", Some(config), vec![]);
        assert_eq!(
            create_payload("codex_side", &codex),
            json!({"patch":{},"providerInstanceMutation":{
                "operation":"create","instanceId":"codex_side","instance":{"driver":"codex","enabled":true,
                "displayName":"Side","accentColor":"#ABCDEF","config":{"binaryPath":"custom","launchArgs":"--verbose","future":{"key":1},"setupMode":"existing"}}
            }})
        );
        let secret = instance(
            "opencode",
            "",
            "",
            Some(json!({"serverPassword":"disposable-fixture","future":true})),
            vec![],
        );
        assert_eq!(
            create_payload("open_test", &secret)["providerInstanceMutation"]["instance"]["config"],
            json!({"serverPassword":"disposable-fixture","future":true})
        );
        for driver in ["cursor", "kiro", "grok"] {
            let plain = instance(
                driver,
                "",
                "",
                Some(json!({})),
                vec![json!({"name":"KIRO_API_KEY","value":"fixture","sensitive":true})],
            );
            assert_eq!(plain.config, None);
            assert_eq!(plain.environment, None); // Web only includes environment for local ACP in this wizard.
        }
    }

    #[test]
    fn local_environment_uses_the_existing_secret_wire_codec() {
        let env = vec![
            json!({"name":"TOKEN","value":{"provider":"vault","reference":"secret/path"},"sensitive":true}),
        ];
        let local = instance(
            "acpRegistry",
            "Local ACP",
            "",
            Some(json!({"source":"local","commandPath":"fixture","commandArgs":["--literal",""]})),
            env.clone(),
        );
        assert_eq!(
            create_payload("acp_local", &local)["providerInstanceMutation"]["instance"]["environment"],
            json!([
                {"name":"TOKEN","value":"","secret":{"provider":"vault","reference":"secret/path"},"sensitive":true}
            ])
        );
        assert_eq!(
            instance("acpRegistry", "", "", Some(json!({"agentId":"test"})), env).environment,
            None
        );
    }
}
