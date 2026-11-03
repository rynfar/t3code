use super::super::auth::{decode_auth_state, decode_install_state};
use super::*;

#[test]
fn install_settlement_ignores_previous_operations_and_failed_runtime() {
    let started = decode_install_state(json!({"phase":"downloading","operationId":"new"})).unwrap();
    let old = decode_install_state(json!({"phase":"succeeded","operationId":"old","installedVersion":"1"})).unwrap();
    assert_eq!(settle_install(&started, Some(&old)), None);
    let failed = decode_install_state(json!({"phase":"failed","operationId":"new","installedVersion":"1"})).unwrap();
    assert_eq!(settle_install(&started, Some(&failed)), Some(false));
    let ready = decode_install_state(json!({"phase":"succeeded","operationId":"new","installedVersion":"1"})).unwrap();
    assert_eq!(settle_install(&started, Some(&ready)), Some(true));
    assert!(!needs_install(&ready));
    let local = decode_install_state(json!({"phase":"idle","source":"local","installedVersion":"1","version":"2"})).unwrap();
    assert!(!needs_install(&local));
    assert!(needs_install(&InstallState { source: Some("managed".into()), ..local }));
}
#[test]
fn pages_and_reconnect_email_belong_to_the_requested_flow_and_method() {
    let a = decode_auth_state(json!({"phase":"waiting","flowId":"mine","authorizationUrl":"https://example.invalid/fake","methods":[{"id":"chatgpt","name":"ChatGPT","accountEmail":"fake@example.invalid"}]})).unwrap();
    assert_eq!(settle_page(Some(&a), Some("other")), None);
    assert_eq!(settle_page(Some(&a), Some("mine")), Some(Some("https://example.invalid/fake".into())));
    let setup = SetupState { auth: Some(a), requested_method: "chatgpt".into(), ..Default::default() };
    assert_eq!(setup.account_description(false, None), "Continue as fake@example.invalid on OpenAI.");
    let ended = AuthState { phase: "cancelled".into(), ..setup.auth.unwrap() };
    assert_eq!(settle_page(Some(&ended), Some("mine")), Some(None));
}
#[test]
fn routing_and_resume_distinguish_local_remote_and_ssh() {
    assert!(local_server("http://127.0.0.1:14658", false));
    assert!(!local_server("http://127.0.0.1:14658", true));
    assert_eq!(start_payload("codex", "chatgpt", "https://example.invalid/welcome", false)["callbackMode"], "client");
    assert_eq!(return_url("https://example.invalid", "env", "codex", true).unwrap(), "https://example.invalid/welcome#agents:env");
    assert_eq!(return_url("https://example.invalid", "env", "codex", false).unwrap(), "https://example.invalid/settings/providers?instanceId=codex");
    assert!(!managed(None));
    assert!(managed(Some(&json!({"setupMode":"managed"}))));
}
#[test]
fn future_handoff_phases_do_not_break_the_stream() {
    assert!(matches!(serde_json::from_value::<HandoffState>(json!({"phase":"future"})).unwrap(), HandoffState::Unknown));
}

#[test]
fn welcome_resume_names_the_environment_and_rejects_other_routes() {
    assert_eq!(resume_environment("t3code://welcome#agents:fixture-env"), Some("fixture-env".into()));
    assert_eq!(resume_environment("https://example.invalid/welcome#agents:fixture-env"), Some("fixture-env".into()));
    assert_eq!(resume_environment("https://example.invalid/threads#agents:fixture-env"), None);
    assert_eq!(resume_environment("t3code://welcome#agents:"), None);
}

#[test]
fn observed_previous_install_cannot_clear_the_owned_busy_state() {
    let mut state = SetupState::default();
    state.started_install(decode_install_state(json!({"phase":"downloading","operationId":"mine"})).unwrap());
    state.observe_install(decode_install_state(json!({"phase":"succeeded","operationId":"old","installedVersion":"1"})).unwrap());
    assert!(state.install_active());
    state.observe_install(decode_install_state(json!({"phase":"succeeded","operationId":"mine","installedVersion":"1"})).unwrap());
    assert!(!state.install_active());
}

#[test]
fn managed_mode_preserves_settings_and_new_accounts_have_independent_instances() {
    let settings =
        serde_json::from_value(json!({"providerInstances":{"codex":{"driver":"codex","config":{"model":"keep","setupMode":"existing"}},"claude":{"driver":"claudeAgent"}}}))
            .unwrap();
    let payload = mode_payload(&settings, "codex", true);
    assert_eq!(payload["providerInstanceMutation"]["instance"]["config"], json!({"model":"keep","enabled":true,"setupMode":"managed"}));
    assert_eq!(payload["providerInstanceMutation"]["instanceId"], "codex");
    let account = new_account_payload("codex_fixture", "  Personal  ").unwrap();
    assert_eq!(account["providerInstanceMutation"]["instanceId"], "codex_fixture");
    assert_eq!(account["providerInstanceMutation"]["instance"]["displayName"], "ChatGPT - Personal");
    assert_eq!(account["providerInstanceMutation"]["instance"]["config"], json!({"enabled":true,"setupMode":"managed"}));
    assert!(new_account_payload("codex_fixture", " ").is_none());
}

#[test]
fn picker_keeps_a_valid_choice_and_falls_back_when_a_saved_account_disappears() {
    let methods = decode_auth_state(
        json!({"phase":"idle","methods":[{"id":"chatgpt","name":"ChatGPT"},{"id":"chatgpt-profile:one","name":"Personal"},{"id":"chatgpt-profile:two","name":"Work"}]}),
    )
    .unwrap()
    .methods
    .unwrap();
    assert_eq!(picker_selection(&methods, ""), "chatgpt-profile:one");
    assert_eq!(picker_selection(&methods, "chatgpt-profile:two"), "chatgpt-profile:two");
    assert_eq!(picker_selection(&methods, "removed"), "chatgpt-profile:one");
    assert_eq!(picker_selection(&methods, "chatgpt-change-account"), "chatgpt-change-account");
    assert_eq!(picker_selection(&[], "removed"), "chatgpt-change-account");
}
