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
    assert_eq!(
        icon_url(&agent("fixture")).as_deref(),
        Some("https://cdn.agentclientprotocol.com/agent.svg")
    );
    let mut fallback = agent("valid-agent");
    fallback.icon = Some("https://example.com/unsafe.svg".into());
    assert_eq!(
        icon_url(&fallback).as_deref(),
        Some("https://cdn.agentclientprotocol.com/registry/v1/latest/valid-agent.svg")
    );
    fallback.id = "../invalid".into();
    assert!(icon_url(&fallback).is_none());
    assert!(external_url(Some("javascript:alert(1)")).is_none());
    assert!(external_url(None).is_none());
    assert_eq!(
        external_url(Some("https://example.com/docs")).as_deref(),
        Some("https://example.com/docs")
    );
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

#[test]
fn recent_queries_survive_dialog_reopen_but_not_expiry_refresh_or_session_changes() {
    let mut cache = RecentSearches::default();
    let scope = SearchScope {
        environment: "https://one".into(),
        connection: 1,
        session: 1,
        permitted: true,
    };
    cache.scope(scope.clone());
    let mut first = Search::default();
    let epoch = first.begin_search();
    first.searched(
        epoch,
        Ok(SearchResult {
            agents: vec![agent("fixture")],
        }),
    );
    cache.remember(&first, 0);
    let mut reopened = Search::default();
    assert!(cache.restore(&mut reopened, 1));
    assert_eq!(reopened.agents, first.agents);
    reopened.typed("other".into());
    assert!(!cache.restore(&mut reopened, 2));
    assert!(reopened.agents.is_none());
    let epoch = reopened.begin_search();
    reopened.searched(epoch, Ok(SearchResult { agents: vec![] }));
    cache.remember(&reopened, 2);
    reopened.typed("  ".into());
    assert!(!reopened.submit());
    assert_eq!(reopened.query, "");
    assert!(cache.restore(&mut reopened, SEARCH_TTL_MS - 1));
    assert!(reopened.submit());
    assert!(!cache.restore(&mut reopened, SEARCH_TTL_MS));
    assert!(reopened.agents.is_some());
    let epoch = reopened.begin_search();
    reopened.searched(epoch, Err("offline".into()));
    cache.remember(&reopened, SEARCH_TTL_MS);
    assert!(!cache.restore(&mut reopened, SEARCH_TTL_MS + 1));
    assert_eq!(reopened.error.as_deref(), Some("offline"));
    cache.touch(&reopened, SEARCH_TTL_MS * 2);
    let mut stale = Search::default();
    assert!(!cache.restore(&mut stale, SEARCH_TTL_MS * 2 + 1));
    assert!(stale.agents.is_some());
    for changed in [
        SearchScope {
            environment: "https://two".into(),
            ..scope.clone()
        },
        SearchScope {
            connection: 2,
            ..scope.clone()
        },
        SearchScope {
            session: 2,
            ..scope.clone()
        },
        SearchScope {
            permitted: false,
            ..scope.clone()
        },
    ] {
        cache.scope(scope.clone());
        cache.remember(&first, 0);
        cache.scope(changed);
        assert!(!cache.restore(&mut Search::default(), 1));
    }
    cache.scope(scope);
    cache.remember(&first, 0);
    assert!(!cache.restore(&mut Search::default(), SEARCH_TTL_MS));
    for n in 0..RECENT_QUERIES + 1 {
        first.typed(n.to_string());
        let epoch = first.begin_search();
        first.searched(epoch, Ok(SearchResult { agents: vec![] }));
        cache.remember(&first, 0);
    }
    assert_eq!(cache.entries.len(), RECENT_QUERIES);
    let mut huge = agent("huge");
    huge.description = "x".repeat(RECENT_BYTES + 1);
    first.typed("huge".into());
    let epoch = first.begin_search();
    first.searched(epoch, Ok(SearchResult { agents: vec![huge] }));
    cache.remember(&first, 0);
    assert!(!cache.restore(&mut first, 1));
    first.typed("0".into());
    assert!(!cache.restore(&mut first, 1));
}
