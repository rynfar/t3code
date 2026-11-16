use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{App, Entity, Global, WeakEntity, Window, actions};
use t3_core::voice_input::{
    DictationKeydown, DictationShortcutAction, DictationShortcutState, DictationTargetSnapshot,
    VoiceInputPhase, VoiceShortcutMode, resolve_dictation_shortcut_owner,
};

use super::controller::VoiceSpeechController;

actions!(t3_voice, [Dictation]);

pub type TargetFocusPredicate = Rc<dyn Fn(&Window, &App) -> bool>;
pub type TargetDisabledPredicate = Rc<dyn Fn(&App) -> bool>;

#[derive(Clone)]
struct RegisteredTarget {
    target_id: u64,
    controller: WeakEntity<VoiceSpeechController>,
    targeted: bool,
    is_focused: TargetFocusPredicate,
    is_disabled: TargetDisabledPredicate,
}

#[derive(Default)]
struct DictationShortcutRegistry {
    next_id: u64,
    targets: Vec<RegisteredTarget>,
    shortcut_state: DictationShortcutState,
    active_target_id: Option<u64>,
    last_keydown_key: String,
    last_keydown_repeat: bool,
}

impl Global for DictationShortcutRegistry {}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn ensure_registry(cx: &mut App) -> &mut DictationShortcutRegistry {
    if cx.try_global::<DictationShortcutRegistry>().is_none() {
        cx.set_global(DictationShortcutRegistry::default());
    }
    cx.global_mut::<DictationShortcutRegistry>()
}

pub fn register_dictation_target(
    controller: &Entity<VoiceSpeechController>,
    targeted: bool,
    is_focused: TargetFocusPredicate,
    is_disabled: TargetDisabledPredicate,
    cx: &mut App,
) -> u64 {
    let weak = controller.downgrade();
    let registry = ensure_registry(cx);
    registry.targets.retain(|entry| entry.controller.upgrade().is_some());
    if let Some(existing) = registry
        .targets
        .iter()
        .find(|entry| entry.controller == weak)
    {
        return existing.target_id;
    }
    registry.next_id += 1;
    let id = registry.next_id;
    registry.targets.push(RegisteredTarget {
        target_id: id,
        controller: weak,
        targeted,
        is_focused,
        is_disabled,
    });
    id
}

pub fn unregister_dictation_target(target_id: u64, cx: &mut App) {
    if cx.try_global::<DictationShortcutRegistry>().is_some() {
        let registry = cx.global_mut::<DictationShortcutRegistry>();
        registry.targets.retain(|entry| entry.target_id != target_id);
        if registry.active_target_id == Some(target_id) {
            registry.active_target_id = None;
        }
    }
}

pub fn record_key_down(key: &str, is_repeat: bool, cx: &mut App) {
    let registry = ensure_registry(cx);
    registry.last_keydown_key = key.to_ascii_lowercase();
    registry.last_keydown_repeat = is_repeat;
}

fn collect_snapshots(
    targets: &[RegisteredTarget],
    window: &Window,
    cx: &App,
) -> Vec<(RegisteredTarget, DictationTargetSnapshot)> {
    targets
        .iter()
        .filter_map(|entry| {
            let entity = entry.controller.upgrade()?;
            let controller = entity.read(cx);
            let snapshot = DictationTargetSnapshot {
                target_id: entry.target_id,
                phase: controller.state().phase,
                focused: (entry.is_focused)(window, cx),
                targeted: entry.targeted,
                available: controller.available(cx),
                disabled: (entry.is_disabled)(cx),
            };
            Some((entry.clone(), snapshot))
        })
        .collect()
}

pub fn handle_dictation_action(window: &mut Window, cx: &mut App) -> bool {
    let Some((entries, owner_id)) = ({
        let entries = ensure_registry(cx)
            .targets
            .iter()
            .filter(|e| e.controller.upgrade().is_some())
            .cloned()
            .collect::<Vec<_>>();
        let paired = collect_snapshots(&entries, window, cx);
        let snapshots: Vec<DictationTargetSnapshot> =
            paired.iter().map(|(_, s)| s.clone()).collect();
        let owner = resolve_dictation_shortcut_owner(&snapshots);
        Some((paired, owner))
    }) else {
        return false;
    };

    let Some(owner_id) = owner_id else {
        return false;
    };
    let Some((target, snap)) = entries.into_iter().find(|(e, _)| e.target_id == owner_id) else {
        return false;
    };
    let Some(controller) = target.controller.upgrade() else {
        return false;
    };

    let mode = crate::settings::store::try_client(cx)
        .map(|doc| doc.voice_shortcut_mode())
        .unwrap_or(VoiceShortcutMode::Auto);

    let action = {
        let registry = ensure_registry(cx);
        let key_code = registry.last_keydown_key.clone();
        let repeat = registry.last_keydown_repeat;
        let action = registry.shortcut_state.on_dictation_keydown(DictationKeydown {
            phase: snap.phase,
            available: snap.available,
            disabled: snap.disabled,
            repeat,
            key_code: &key_code,
            now_ms: now_ms(),
            mode,
        });
        if action == DictationShortcutAction::Start {
            registry.active_target_id = Some(owner_id);
        }
        action
    };

    match action {
        DictationShortcutAction::Start => {
            controller.update(cx, |voice, cx| voice.start(window, cx));
            true
        }
        DictationShortcutAction::Stop => {
            controller.update(cx, |voice, cx| voice.stop(cx));
            true
        }
        DictationShortcutAction::Cancel => {
            controller.update(cx, |voice, cx| voice.cancel(cx));
            true
        }
        DictationShortcutAction::None => false,
    }
}

pub fn handle_key_up(key: &str, cx: &mut App) {
    let key_lower = key.to_ascii_lowercase();
    if cx.try_global::<DictationShortcutRegistry>().is_none() {
        return;
    }
    let (action, target) = {
        let registry = cx.global_mut::<DictationShortcutRegistry>();
        let action = registry
            .shortcut_state
            .on_keyup_or_blur(Some(&key_lower), now_ms());
        let target = if action == DictationShortcutAction::Stop {
            registry.active_target_id.take().and_then(|target_id| {
                registry
                    .targets
                    .iter()
                    .find(|t| t.target_id == target_id)
                    .and_then(|t| t.controller.upgrade())
            })
        } else {
            None
        };
        (action, target)
    };
    if action == DictationShortcutAction::Stop
        && let Some(target) = target
    {
        target.update(cx, |voice, cx| voice.stop(cx));
    }
}

pub fn handle_window_blur(cx: &mut App) {
    if cx.try_global::<DictationShortcutRegistry>().is_none() {
        return;
    }
    let (action, target) = {
        let registry = cx.global_mut::<DictationShortcutRegistry>();
        let action = registry.shortcut_state.on_keyup_or_blur(None, now_ms());
        let target = if action == DictationShortcutAction::Stop {
            registry.active_target_id.take().and_then(|target_id| {
                registry
                    .targets
                    .iter()
                    .find(|t| t.target_id == target_id)
                    .and_then(|t| t.controller.upgrade())
            })
        } else {
            None
        };
        (action, target)
    };
    if action == DictationShortcutAction::Stop
        && let Some(target) = target
    {
        target.update(cx, |voice, cx| voice.stop(cx));
    }
}

/// Cancels an active voice dictation on Escape before popups/composer escape handling.
pub fn handle_escape(window: &Window, cx: &mut App) -> bool {
    let entries = ensure_registry(cx)
        .targets
        .iter()
        .filter(|e| e.controller.upgrade().is_some())
        .cloned()
        .collect::<Vec<_>>();
    let paired = collect_snapshots(&entries, window, cx);
    let busy_target = paired.iter().find(|(_, snap)| {
        !matches!(snap.phase, VoiceInputPhase::Idle | VoiceInputPhase::Error)
    });
    let Some((target, snap)) = busy_target else {
        return false;
    };
    let Some(controller) = target.controller.upgrade() else {
        return false;
    };
    let action = ensure_registry(cx)
        .shortcut_state
        .on_escape(snap.phase, false);
    if action == DictationShortcutAction::Cancel {
        ensure_registry(cx).active_target_id = None;
        controller.update(cx, |voice, cx| voice.cancel(cx));
        true
    } else {
        false
    }
}

pub fn active_setup_controller(cx: &App) -> Option<Entity<VoiceSpeechController>> {
    let registry = cx.try_global::<DictationShortcutRegistry>()?;
    registry.targets.iter().find_map(|entry| {
        let entity = entry.controller.upgrade()?;
        entity.read(cx).setup().open.then_some(entity)
    })
}

pub fn is_setup_modal_open(cx: &App) -> bool {
    active_setup_controller(cx).is_some()
}

/// Closes an open VoiceInputSetup modal on Escape when not actively downloading, or consumes
/// Escape while a download is active so background views don't handle it.
pub fn handle_setup_escape(cx: &mut App) -> bool {
    let Some(controller) = active_setup_controller(cx) else {
        return false;
    };
    if !controller.read(cx).setup().downloading {
        controller.update(cx, |voice, cx| voice.setup_set_open(false, cx));
    }
    true
}

pub fn dictation_shortcut_label(cx: &App) -> String {
    crate::keymap::label("composer.dictation", cx).unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "⌘⇧D".to_owned()
        } else {
            "Ctrl+Shift+D".to_owned()
        }
    })
}
