//! The HTML host's ownership, activity and controller boundary (`BrowserDocumentFrame.tsx`).
use super::host::{self, KeyDown, LinkRequest};
use crate::keybindings::{ResolvedKeybindingRule, ShortcutMatchContext};
use crate::pull_requests::target::{Project, Target};

pub const SANDBOX: &str = "sandbox allow-scripts allow-forms";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageOrigin {
    pub environment: String,
    pub thread: String,
}
impl PageOrigin {
    pub fn for_view(environment: Option<&str>, selected: Option<&str>, embedded: Option<&str>) -> Option<Self> {
        Some(Self { environment: environment?.into(), thread: embedded.or(selected)?.into() })
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Link(LinkRequest),
    Key(KeyDown),
    RawNavigation(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    External(String),
    Preview { owner: PageOrigin, url: String },
    ChangeRequest { owner: PageOrigin, target: Target },
    Shortcut(String),
}

pub struct Context<'a> {
    pub origin: &'a PageOrigin,
    pub current_origin: Option<&'a PageOrigin>,
    pub active_environment: Option<&'a str>,
    pub focused: bool,
    pub activated: bool,
    pub prefer_app: bool,
    pub can_preview: bool,
    pub open_change_requests: bool,
    pub projects: &'a [Project<'a>],
    pub keybindings: &'a [ResolvedKeybindingRule],
    pub shortcut_context: &'a ShortcutMatchContext,
    pub mac: bool,
}

pub fn route(event: &Event, context: &Context<'_>) -> Option<Action> {
    if context.current_origin != Some(context.origin) || context.active_environment != Some(context.origin.environment.as_str()) || !context.focused || !context.activated {
        return None;
    }
    match event {
        Event::RawNavigation(_) => None,
        Event::Link(link) => {
            if !host::web_url(&link.url) {
                return None;
            }
            if link.meta_key || link.ctrl_key {
                return Some(Action::External(link.url.clone()));
            }
            if context.open_change_requests
                && let Some(target) =
                    crate::pull_requests::target::resolve(&link.url, context.projects, Some(&context.origin.environment), None, Some(&context.origin.environment), true)
            {
                return Some(Action::ChangeRequest { owner: context.origin.clone(), target });
            }
            if context.prefer_app && context.can_preview {
                Some(Action::Preview { owner: context.origin.clone(), url: link.url.clone() })
            } else {
                Some(Action::External(link.url.clone()))
            }
        }
        Event::Key(key) => host::shortcut_command(key, context.keybindings, context.shortcut_context, context.mac).map(|command| Action::Shortcut(command.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::{ShortcutMatchContext, default_resolved_keybindings};
    use serde_json::json;

    #[test]
    fn html_routing_uses_side_chat_owner_and_rejects_stale_unfocused_and_raw_events() {
        let owner = PageOrigin::for_view(Some("e"), Some("main-A"), Some("side-B")).unwrap();
        assert_eq!(owner.thread, "side-B");
        let shortcuts = ShortcutMatchContext::default();
        let mut context = Context {
            origin: &owner,
            current_origin: Some(&owner),
            active_environment: Some("e"),
            focused: true,
            activated: true,
            prefer_app: true,
            can_preview: true,
            open_change_requests: true,
            projects: &[],
            keybindings: default_resolved_keybindings(),
            shortcut_context: &shortcuts,
            mac: true,
        };
        let link = Event::Link(host::read_link(&json!({"jsonrpc":"2.0","method":"ui/open-link","id":1,"params":{"url":"https://example.com/"}})).unwrap());
        assert_eq!(route(&link, &context), Some(Action::Preview { owner: owner.clone(), url: "https://example.com/".into() }));
        let other = PageOrigin { environment: "e".into(), thread: "main-A".into() };
        for current in [None, Some(&other)] {
            context.current_origin = current;
            assert_eq!(route(&link, &context), None);
        }
        context.current_origin = Some(&owner);
        context.active_environment = Some("other");
        assert_eq!(route(&link, &context), None);
        context.active_environment = Some("e");
        context.focused = false;
        assert_eq!(route(&link, &context), None);
        context.focused = true;
        context.activated = false;
        assert_eq!(route(&link, &context), None);
        context.activated = true;
        for url in ["https://example.com/", "javascript:alert(1)"] {
            assert_eq!(route(&Event::RawNavigation(url.into()), &context), None);
        }
        let key = Event::Key(host::read_key(&json!({"jsonrpc":"2.0","method":"t3/notifications/keydown","params":{"key":"k","code":"KeyK","metaKey":true}})).unwrap());
        assert_eq!(route(&key, &context), Some(Action::Shortcut("commandPalette.toggle".into())));
        context.focused = false;
        assert_eq!(route(&key, &context), None);
    }

    #[test]
    fn html_routing_selects_readable_pr_or_falls_through_to_browser_preference() {
        let owner = PageOrigin { environment: "e".into(), thread: "side-B".into() };
        let shortcuts = ShortcutMatchContext::default();
        let identity = json!({"provider":"github","canonicalKey":"github.com/acme/app","displayName":"acme/app"});
        let projects = [Project { id: "project-B", environment: "e", identity: Some(&identity), config_loaded: true }];
        let mut context = Context {
            origin: &owner,
            current_origin: Some(&owner),
            active_environment: Some("e"),
            focused: true,
            activated: true,
            prefer_app: true,
            can_preview: true,
            open_change_requests: true,
            projects: &projects,
            keybindings: default_resolved_keybindings(),
            shortcut_context: &shortcuts,
            mac: true,
        };
        let mut link = host::read_link(&json!({"jsonrpc":"2.0","method":"ui/open-link","id":1,"params":{"url":"https://github.com/acme/app/pull/7"}})).unwrap();
        let Some(Action::ChangeRequest { owner: target_owner, target }) = route(&Event::Link(link.clone()), &context) else {
            panic!("expected a readable PR");
        };
        assert_eq!(target_owner, owner);
        assert_eq!(target.reference.project_id, "project-B");
        link.meta_key = true;
        assert_eq!(route(&Event::Link(link.clone()), &context), Some(Action::External(link.url.clone())));
        link.meta_key = false;
        context.open_change_requests = false;
        assert!(matches!(route(&Event::Link(link.clone()), &context), Some(Action::Preview { owner:target_owner, .. }) if target_owner == owner));
        context.open_change_requests = true;
        link.url = "https://unconfigured.example/acme/app/pull/7".into();
        assert!(matches!(route(&Event::Link(link.clone()), &context), Some(Action::Preview { .. })));
        context.can_preview = false;
        assert_eq!(route(&Event::Link(link.clone()), &context), Some(Action::External(link.url.clone())));
        context.can_preview = true;
        context.prefer_app = false;
        assert_eq!(route(&Event::Link(link.clone()), &context), Some(Action::External(link.url.clone())));
        link.url = "file:///etc/hosts".into();
        assert_eq!(route(&Event::Link(link), &context), None);
    }
}
