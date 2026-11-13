//! Offscreen-only fixture; its confirmation never dispatches a server mutation.
use super::*;
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    #[serde(default)]
    providers: Vec<t3_core::server_config::ServerProvider>,
    #[serde(default)]
    plan: Plan,
    #[serde(default)]
    dialog: bool,
    #[serde(default)]
    counts: Vec<(String, usize)>,
    #[serde(default)]
    busy: Option<String>,
    #[serde(default)]
    run: Option<UpdateEverythingRun>,
    #[serde(default)]
    stages: Vec<(String, Option<ServerUpdateStage>)>,
    #[serde(default)]
    capsule_open: bool,
    #[serde(default)]
    now_ms: Option<i64>,
}
pub(super) fn show(value: serde_json::Value, window: &mut Window, cx: &mut App) {
    let fixture: Fixture = serde_json::from_value(value).expect("valid update fixture");
    let Some(control) = cx.try_global::<Host>().and_then(|h| h.control.upgrade()) else {
        return;
    };
    control.update(cx, |control, cx| {
        control.fixture = true;
        control.success_timer = None;
        control.capsule_timer = None;
        control.handoff_timer = None;
        control.pill = updates::provider_pill(&fixture.providers, None, &HashSet::new());
        control.plan = fixture.plan.clone();
        control.stages = fixture.stages;
        control.capsule_open = fixture.capsule_open;
        control.fixture_now_ms = fixture.now_ms;
        control.run = fixture.run.or_else(|| {
            fixture.busy.as_ref().map(|label| UpdateEverythingRun {
                track_switch: false,
                phase: updates::UpdateRunPhase::Servers,
                machines: vec![updates::UpdateRunMachine {
                    environment_id: "fixture".into(),
                    label: label.clone(),
                    updates_providers: false,
                    server: None,
                    handed_off_version: None,
                    started_at_ms: fixture.now_ms.unwrap_or_else(now_ms),
                    result: None,
                    failure: None,
                    provider_failure: None,
                }],
            })
        });
        if fixture.dialog {
            control.dialog.update(cx, |dialog, cx| {
                dialog.open(fixture.plan, true, window, cx);
                dialog.counts = fixture.counts;
            });
        }
        cx.notify();
    });
}
