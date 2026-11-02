use serde_json::json;

use super::*;
use crate::server_config::ServerSettings;

fn decode(value: Value) -> WorktreeSettings {
    serde_json::from_value::<ServerSettings>(value).unwrap().worktrees
}

#[test]
fn worktree_settings_take_contract_defaults() {
    let settings = decode(json!({}));
    assert_eq!(settings, WorktreeSettings::default());
    assert_eq!(settings.branch_name_prefix, "t3");
    assert_eq!(settings.effective_submodules(), Submodules::Recursive);
    assert_eq!(settings.cleanup(), CleanupRules { worktree_keep_when: KeepWhen::UncommittedChanges, ..CleanupRules::default() });
}

#[test]
fn worktree_settings_drop_bad_values_one_at_a_time() {
    let settings = decode(json!({
        "worktreesDirectory": "  ~/worktrees ",
        "worktreeSubmodules": "future-mode",
        "branchNamingMode": "semantic",
        "branchNamePrefix": "",
        "storageCleanup": {
            "worktreeKeepWhen": "bogus",
            "worktreeAfterDays": 0,
            "worktreeSettledAfterDays": 0,
            "worktreeOnMerge": true,
            "worktreeUnchanged": "yes",
            "logsAfterDays": 4,
        },
    }));
    assert_eq!(settings.worktrees_directory, "~/worktrees");
    assert_eq!(settings.worktree_submodules, None);
    assert_eq!(settings.branch_naming_mode, BranchNaming::Semantic);
    assert_eq!(settings.branch_name_prefix, "", "an empty prefix is a choice, not a missing value");
    let rules = settings.storage_cleanup;
    assert_eq!(rules.worktree_keep_when, KeepWhen::UncommittedChanges);
    assert_eq!(rules.worktree_after_days, None, "inactive retention starts at one day");
    assert_eq!(rules.worktree_settled_after_days, Some(0), "settled retention may be zero days");
    assert!(rules.worktree_on_merge);
    assert!(!rules.worktree_unchanged);
}

#[test]
fn a_cleanup_policy_decides_what_the_rows_show() {
    let storage = json!({ "worktreeKeepWhen": "tracked-changes", "worktreeOnMerge": true, "worktreeAfterDays": 30 });
    let off = decode(json!({ "storageCleanup": storage, "worktreeCleanup": { "mode": "off" } }));
    assert_eq!(off.cleanup(), CleanupRules { worktree_keep_when: KeepWhen::TrackedChanges, ..CleanupRules::default() });

    let custom = decode(json!({
        "storageCleanup": storage,
        "worktreeCleanup": { "mode": "custom", "rules": { "worktreeOnDelete": true, "worktreeAfterDays": 5 } },
    }));
    assert_eq!(custom.cleanup(), CleanupRules { worktree_on_delete: true, worktree_after_days: Some(5), ..CleanupRules::default() });

    let unknown = decode(json!({ "storageCleanup": storage, "worktreeCleanup": { "mode": "later" } }));
    assert_eq!(unknown.cleanup().worktree_after_days, Some(30));
}

#[test]
fn worktree_changes_patch_only_their_key() {
    let payload = |change: WorktreeChange| change.into_server().payload();
    assert_eq!(payload(WorktreeChange::Directory(" ~/wt ".into())), json!({ "patch": { "worktreesDirectory": "~/wt" } }));
    assert_eq!(payload(WorktreeChange::Submodules(None)), json!({ "patch": { "worktreeSubmodules": null } }));
    assert_eq!(payload(WorktreeChange::Submodules(Some(Submodules::TopLevel))), json!({ "patch": { "worktreeSubmodules": "top-level" } }));
    assert_eq!(payload(WorktreeChange::BranchNaming(BranchNaming::Custom)), json!({ "patch": { "branchNamingMode": "custom" } }));
    assert_eq!(
        payload(WorktreeChange::Cleanup(CleanupRule::AfterDays(Some(9_999)))),
        json!({ "patch": { "storageCleanup": { "worktreeAfterDays": 3650 } } })
    );
    assert_eq!(
        payload(WorktreeChange::Cleanup(CleanupRule::SettledAfterDays(None))),
        json!({ "patch": { "storageCleanup": { "worktreeSettledAfterDays": null } } })
    );
    assert_eq!(
        payload(WorktreeChange::Cleanup(CleanupRule::KeepWhen(KeepWhen::AnyLocalFiles))),
        json!({ "patch": { "storageCleanup": { "worktreeKeepWhen": "any-local-files" } } })
    );
    assert_eq!(clamp_retention(0.4, 1), 1);
    assert_eq!(clamp_retention(0.4, 0), 0);
    assert_eq!(clamp_retention(12.6, 1), 13);
    assert_eq!(clamp_retention(1e9, 0), MAX_RETENTION_DAYS);
}
