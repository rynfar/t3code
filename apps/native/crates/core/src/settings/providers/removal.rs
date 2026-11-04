//! `ProviderSettingsPanel.tsx` removal/reset confirmation, bound to the captured connection
//! and saved instance. A dismissed or stale confirmation cannot produce a mutation.

use serde_json::Value;

use super::{find_driver, managed_acp_agent, remove_payload};
use crate::providers::resolve_instance_enabled;
use crate::server_config::{ProviderInstanceConfig, ServerSettings};

#[derive(Clone, Copy)]
pub struct RemovalContext<'a> {
    pub connection: Option<u64>,
    pub revision: u64,
    pub settings: Option<&'a ServerSettings>,
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemovalPlan {
    pub instance_id: String,
    pub connection: u64,
    pub revision: u64,
    pub reset: bool,
    instance: ProviderInstanceConfig,
}

impl RemovalPlan {
    fn current(&self, context: &RemovalContext<'_>) -> bool {
        context.allowed
            && context.connection == Some(self.connection)
            && context.revision == self.revision
            && context
                .settings
                .and_then(|s| s.provider_instances.get(&self.instance_id))
                == Some(&self.instance)
    }

    pub fn question(&self) -> String {
        let name = self
            .instance
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                find_driver(&self.instance.driver)
                    .map_or(self.instance.driver.as_str(), |driver| driver.label)
            });
        if self.reset {
            format!("Reset the {name} provider to its defaults?")
        } else {
            format!("Remove the {name} provider instance?")
        }
    }

    pub fn warning(&self) -> String {
        let mut warning =
            "Its saved settings, including any custom binary path, will be removed.".to_owned();
        let defaults = ProviderInstanceConfig {
            driver: self.instance.driver.clone(),
            ..Default::default()
        };
        if self.reset
            && resolve_instance_enabled(&self.instance)
            && !resolve_instance_enabled(&defaults)
        {
            warning.push_str(" It will also be turned off.");
        }
        warning
    }

    pub fn payload(&self) -> Value {
        remove_payload(&self.instance_id)
    }

    pub fn managed_agent(&self) -> Option<String> {
        if self.reset {
            None
        } else {
            managed_acp_agent(&self.instance)
        }
    }

    pub fn failure_title(&self) -> &'static str {
        if self.reset {
            "Could not reset provider instance"
        } else {
            "Could not delete provider instance"
        }
    }
}

#[derive(Default)]
pub struct RemovalState {
    confirmation: Option<RemovalPlan>,
    pending: Option<RemovalPlan>,
}

impl RemovalState {
    pub fn confirmation(&self) -> Option<&RemovalPlan> {
        self.confirmation.as_ref()
    }
    pub fn busy(&self) -> bool {
        self.confirmation.is_some() || self.pending.is_some()
    }

    pub fn request(
        &mut self,
        instance_id: &str,
        reset: bool,
        context: &RemovalContext<'_>,
    ) -> bool {
        if self.busy() || !context.allowed {
            return false;
        }
        let Some(connection) = context.connection else {
            return false;
        };
        let Some(instance) = context
            .settings
            .and_then(|s| s.provider_instances.get(instance_id))
        else {
            return false;
        };
        let is_default = find_driver(instance_id).is_some();
        if reset != is_default {
            return false;
        }
        self.confirmation = Some(RemovalPlan {
            instance_id: instance_id.into(),
            connection,
            revision: context.revision,
            reset,
            instance: instance.clone(),
        });
        true
    }

    pub fn cancel(&mut self) {
        self.confirmation = None;
    }

    pub fn stale(&self, context: &RemovalContext<'_>) -> bool {
        self.confirmation
            .as_ref()
            .is_some_and(|plan| !plan.current(context))
    }

    pub fn confirm(&mut self, context: &RemovalContext<'_>) -> Option<RemovalPlan> {
        let plan = self.confirmation.take()?;
        if self.pending.is_some() || !plan.current(context) {
            return None;
        }
        self.pending = Some(plan.clone());
        Some(plan)
    }

    pub fn finish(&mut self, plan: &RemovalPlan) -> bool {
        if self.pending.as_ref() != Some(plan) {
            return false;
        }
        self.pending = None;
        true
    }
}
