use super::*;
use std::collections::HashMap;
fn agent(id: &str) -> Agent {
    serde_json::from_value(json!({"id":id,"name":"Fixture Agent","version":"old","description":"Fixture","website":null,"repository":null,"icon":"https://cdn.agentclientprotocol.com/agent.svg","distribution":"binary","future":true})).unwrap()
}
#[test]
fn search_retains_results_during_refresh_filters_real_metadata_and_retires_stale_work() {
    let mut search = Search::default();
    let first = search.begin_search();
    assert!(search.pending && search.agents.is_none());
    let mut agents: Vec<_> = FIRST_CLASS_IDS.iter().map(|id| agent(id)).collect();
    agents.push(agent("fixture"));
    assert!(search.searched(first, Ok(SearchResult { agents })));
    assert_eq!(search.agents.as_ref().unwrap(), &[agent("fixture")]);
    search.typed("  find  ".into());
    let refresh = search.begin_search();
    assert_eq!(search.submitted, "find");
    assert!(search.pending && search.agents.is_some());
    assert!(!search.searched(first, Ok(SearchResult { agents: vec![] })));
    assert!(search.searched(refresh, Err("offline".into())));
    assert_eq!(search.error.as_deref(), Some("offline"));
    search.retire();
    assert!(!search.searched(refresh, Ok(SearchResult { agents: vec![] })));
    assert_eq!(search_payload(" "), json!({"query":"","limit":512}));
    assert_eq!(search_payload(" find "), json!({"query":"find"}));
    let ticket = search.begin_search();
    assert!(search.searched(ticket, Ok(SearchResult { agents: vec![] })));
    assert!(search.agents.as_ref().unwrap().is_empty());
    let ticket = search.begin_search();
    search.searched(
        ticket,
        Ok(SearchResult {
            agents: vec![agent("fixture"); MAX_AGENTS + 1],
        }),
    );
    assert_eq!(search.agents.as_ref().unwrap().len(), MAX_AGENTS);
}
#[test]
fn prepare_merges_server_identity_before_defaults_and_cannot_cross_hidden_step() {
    assert_eq!(
        instance_id("ACP Registry", &["acpRegistry_acp_registry".into()]),
        "acpRegistry_acp_registry_2"
    );
    let mut search = Search::default();
    let ticket = search.begin_prepare("fixture").unwrap();
    assert!(search.begin_prepare("other").is_none());
    let prepared = || {
        Ok(Prepared {
            agent_id: "canonical".into(),
            version: "new".into(),
            distribution: "npx".into(),
            prepared: true,
        })
    };
    search.retire();
    assert!(
        search
            .prepared(ticket, agent("fixture"), prepared())
            .is_none()
    );
    let ticket = search.begin_prepare("fixture").unwrap();
    let selected = search
        .prepared(ticket, agent("fixture"), prepared())
        .unwrap();
    assert_eq!(
        (&*selected.id, &*selected.version, &*selected.distribution),
        ("canonical", "new", "npx")
    );
    assert_eq!(
        selected.config(),
        json!({"agentId":"canonical","distribution":"auto","registryIconUrl":"https://cdn.agentclientprotocol.com/agent.svg"})
    );
    let ticket = search.begin_prepare("fixture").unwrap();
    assert!(
        search
            .prepared(ticket, agent("fixture"), Err("failed".into()))
            .is_none()
    );
    assert_eq!(search.prepare_error.as_deref(), Some("failed"));
    let mut instances = HashMap::new();
    instances.insert(
        "local".to_owned(),
        ProviderInstanceConfig {
            driver: ACP_REGISTRY.into(),
            config: Some(json!({"source":"local","agentId":"canonical"})),
            ..Default::default()
        },
    );
    assert!(!configured(instances.values(), "canonical"));
    instances.insert(
        "managed".to_owned(),
        ProviderInstanceConfig {
            driver: ACP_REGISTRY.into(),
            config: Some(selected.config()),
            ..Default::default()
        },
    );
    assert!(configured(instances.values(), "canonical"));
    for url in [
        "http://cdn.agentclientprotocol.com/x",
        "https://cdn.agentclientprotocol.com.evil/x",
        "https://user@cdn.agentclientprotocol.com/x",
        "https://cdn.agentclientprotocol.com:444/x",
    ] {
        assert!(official_icon(url).is_none());
    }
}
#[test]
fn wizard_discovery_and_finish_follow_auth_state() {
    assert!(discovering(false, None, None));
    assert!(!discovering(false, None, Some("offline")));
    let state = |phase: &str, methods: Value| {
        super::super::auth::decode_auth_state(json!({"phase":phase,"methods":methods})).unwrap()
    };
    assert!(discovering(false, Some(&state("idle", Value::Null)), None));
    assert!(!discovering(false, Some(&state("idle", json!([]))), None));
    let mut provider: ServerProvider = serde_json::from_value(json!({"instanceId":"fixture","driver":"acpRegistry","installed":true,"enabled":true,"status":"ready","auth":{"status":"unknown"},"models":[]})).unwrap();
    assert!(signed_in(
        Some(&provider),
        Some(&state("succeeded", json!([])))
    ));
    provider.auth.status = "unauthenticated".into();
    assert!(!signed_in(
        Some(&provider),
        Some(&state("succeeded", json!([])))
    ));
}
