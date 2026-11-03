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
}
pub(super) fn show(value: serde_json::Value, window: &mut Window, cx: &mut App) {
    let fixture: Fixture = serde_json::from_value(value).expect("valid update fixture");
    let Some(control) = cx.try_global::<Host>().and_then(|h| h.control.upgrade()) else {
        return;
    };
    control.update(cx, |control, cx| {
        control.fixture = true;
        control.success_timer = None;
        control.pill = updates::provider_pill(&fixture.providers, None, &HashSet::new());
        control.plan = fixture.plan.clone();
        control.busy = fixture.busy;
        if fixture.dialog {
            control.dialog.update(cx, |dialog, cx| {
                dialog.open(fixture.plan, true, window, cx);
                dialog.counts = fixture.counts;
            });
        }
        cx.notify();
    });
}
