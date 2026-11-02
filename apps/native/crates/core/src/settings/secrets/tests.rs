use serde_json::json;

use super::*;
use crate::auth::{AuthDescriptor, AuthSessionState};

fn providers(value: serde_json::Value) -> SecretProviders {
    serde_json::from_value(value).unwrap()
}

fn draft(name: &str, account: &str) -> Draft {
    Draft { name: name.to_owned(), account: account.to_owned() }
}

fn session(scopes: &[&str]) -> AuthSessionState {
    AuthSessionState {
        authenticated: true,
        auth: AuthDescriptor { server_update_scope: Some("environment:maintain".into()) },
        scopes: Some(scopes.iter().map(|scope| (*scope).to_owned()).collect()),
        permissions: None,
    }
}

#[test]
fn decodes_entries_and_drops_ones_the_contract_rejects() {
    let decoded = providers(json!({
        "personal": { "kind": "one_password", "label": "Personal", "config": { "account": "my.1password.com" } },
        "vault": { "kind": "openbao", "label": " Vault " },
        "-bad-id": { "kind": "one_password", "label": "Bad" },
        "nolabel": { "kind": "one_password", "label": "  " },
        "badkind": { "kind": "one password", "label": "Bad kind" },
    }));
    assert_eq!(decoded.ids(), vec!["personal".to_owned(), "vault".to_owned()]);
    assert_eq!(decoded.get("vault").unwrap().label, "Vault");
    assert_eq!(providers(json!(null)), SecretProviders::default());
}

#[test]
fn classifies_one_password_accounts_and_shows_the_rest_read_only() {
    let decoded = providers(json!({
        "a": { "kind": "one_password", "label": "A", "config": { "account": "ok.acct" } },
        "b": { "kind": "one_password", "label": "B", "config": { "account": "bad acct" } },
        "c": { "kind": "one_password", "label": "C" },
        "d": { "kind": "openbao", "label": "D", "config": { "account": "ok" } },
    }));
    let classified: Vec<Classified> = decoded.0.iter().map(classify).collect();
    assert_eq!(classified, vec![Classified::OnePassword { account: "ok.acct".into() }, Classified::Unsupported, Classified::Unsupported, Classified::Unsupported]);
    assert_eq!(unsupported_description(decoded.get("d").unwrap()), "Unsupported provider (openbao)");
    assert_eq!(draft_from(decoded.get("a").unwrap()), draft("A", "ok.acct"));
    assert_eq!(draft_from(decoded.get("d").unwrap()), draft("D", ""));
}

#[test]
fn ids_follow_the_name_as_a_slug() {
    assert_eq!(id_from_name("Personal").as_deref(), Some("personal"));
    assert_eq!(id_from_name("---Hello---World---").as_deref(), Some("hello-world"));
    assert_eq!(id_from_name("Ünïcode").as_deref(), Some("n-code"));
    assert_eq!(id_from_name("1Password Work").as_deref(), Some("1password-work"));
    assert_eq!(id_from_name("!!!"), None);
    let long = format!("{}-b", "a".repeat(63));
    assert_eq!(id_from_name(&long).unwrap(), "a".repeat(63));
}

/// Each case is what `secretProviderFromDraft` answers for the same draft.
#[test]
fn draft_validation_matches_the_web() {
    let existing = vec!["personal".to_owned()];
    let fails = |name: &str, account: &str| provider_from_draft(&draft(name, account), &existing, &Target::Add).unwrap_err();
    assert_eq!(fails("", ""), Issues { name: Some("Enter a name.".into()), account: Some("Enter the 1Password account.".into()) });
    assert_eq!(fails("Personal", "my.1password.com"), Issues { name: Some("A secret provider with this name exists.".into()), account: None });
    let pattern = "1Password account must be a shorthand, sign-in address, or ID with no spaces.";
    assert_eq!(fails("  ", "-bad"), Issues { name: Some("Enter a name.".into()), account: Some(pattern.into()) });
    assert_eq!(fails("!!!", "has space"), Issues { name: Some("The name needs at least one letter or digit.".into()), account: Some(pattern.into()) });
    assert_eq!(fails("Work", "x@y.z").account.as_deref(), Some(pattern));
    assert_eq!(fails("Work", &"A".repeat(254)).account.as_deref(), Some("Expected a value with a length of at most 253"));
    assert_eq!(fails(&"a".repeat(70), "ok").name.as_deref(), Some("Expected a value with a length of at most 64"));

    let (id, config) = provider_from_draft(&draft("  Work Stuff ", " my.1password.com "), &existing, &Target::Add).unwrap();
    assert_eq!(id, "work-stuff");
    assert_eq!(config, json!({ "kind": "one_password", "label": "Work Stuff", "config": { "account": "my.1password.com" } }));
}

#[test]
fn editing_keeps_the_id_even_when_the_name_changes() {
    let existing = vec!["personal".to_owned()];
    let (id, config) = provider_from_draft(&draft("Renamed!", "a"), &existing, &Target::Edit("personal".into())).unwrap();
    assert_eq!(id, "personal");
    assert_eq!(config["label"], "Renamed!");
    // A name with no letters still saves under the id it already has.
    assert!(provider_from_draft(&draft("!!!", "a"), &existing, &Target::Edit("personal".into())).is_ok());
}

#[test]
fn payloads_add_replace_and_remove_one_entry() {
    assert_eq!(save_payload("work", json!({ "kind": "one_password" })), json!({ "patch": { "secretProviders": { "work": { "kind": "one_password" } } } }));
    assert_eq!(remove_payload("work"), json!({ "patch": { "secretProviders": { "work": null } } }));
}

#[test]
fn page_state_follows_connection_capability_and_grant() {
    let manage = session(&["providers:manage"]);
    let read = session(&["orchestration:read"]);
    assert_eq!(page_state(false, true, Some(true), Some(&manage)), PageState::Disconnected);
    assert_eq!(page_state(true, true, None, Some(&manage)), PageState::UpdateRequired);
    assert_eq!(page_state(true, true, Some(true), Some(&manage)), PageState::Providers { editable: true });
    assert_eq!(page_state(true, true, Some(true), Some(&read)), PageState::Providers { editable: false });
    // Grants still loading, or no config yet: read-only until known.
    assert_eq!(page_state(true, true, Some(true), None), PageState::Providers { editable: false });
    assert_eq!(page_state(true, false, None, Some(&manage)), PageState::Providers { editable: false });
}

/// Outputs of the web's `redactedPlaceholder` for the same inputs.
#[test]
fn redacted_placeholder_matches_the_web() {
    for (value, redacted) in [
        ("my.1password.com", "9u.xknfjcab9.sde"),
        ("trev@example.com", "ryav@258kkgv.fwc"),
        ("ABCDEFGHIJKLMNOP", "xddjtqtwxdrsa5fr"),
        ("x", "5"),
        ("a-b_c.d", "b-y_e.4"),
        ("h\u{e9}llo\u{1f600}", "8hvdh7"),
    ] {
        assert_eq!(redacted_placeholder(value), redacted, "{value}");
    }
}

#[test]
fn an_edit_closes_once_its_provider_is_removed_elsewhere() {
    let present = providers(json!({ "personal": { "kind": "one_password", "label": "Personal", "config": { "account": "a" } } }));
    let removed = providers(json!({}));
    let edit = Target::Edit("personal".into());
    assert!(edit.is_open_in(&present));
    assert!(!edit.is_open_in(&removed), "Save would write the removed provider back");
    assert!(Target::Add.is_open_in(&removed));
}
