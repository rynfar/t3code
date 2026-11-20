use serde_json::json;

use super::stream::*;
use super::*;

fn host(id: &str, kind: &str, platforms: serde_json::Value) -> serde_json::Value {
    json!({ "id": id, "label": format!("{id} host"), "kind": kind, "platforms": platforms, "hubInstalled": true, "agentDeviceInstalled": false })
}

fn device(host: &str, id: &str, platform: &str, name: &str, booted: bool) -> serde_json::Value {
    json!({ "hostId": host, "id": id, "platform": platform, "name": name, "version": "18.2", "booted": booted, "physical": false })
}

fn state(patch: serde_json::Value) -> ServiceState {
    let mut value = json!({
        "hostStatus": "ready",
        "hostStatuses": {},
        "hosts": [host("local", "local", json!([{ "platform": "ios", "available": true }, { "platform": "android", "available": false, "reason": "No Android SDK." }]))],
        "devices": [],
        "sessions": [],
        "onboardingCompleted": true,
        "agentAccessEnabled": false,
        "hubBasePath": "/api/device-hub",
        "revision": 3,
    });
    for (key, field) in patch.as_object().unwrap() {
        value[key] = field.clone();
    }
    ServiceState::decode(value).unwrap()
}

#[test]
fn decodes_the_wire_state_merging_cloud_hosts_and_skipping_bad_entries() {
    let decoded = state(json!({
        "easHosts": [host("eas", "eas", json!([{ "platform": "ios", "available": true }]))],
        "simlockHosts": [host("lab", "simlock", json!([]))],
        "devices": [device("local", "a", "ios", "iPhone 16", true), device("local", "w", "watchos", "Watch", true), json!({ "id": "broken" })],
        "hostStatuses": { "local": { "status": "ready", "detail": "slow" }, "eas": { "status": "hibernating" }, "bad": 4 },
        "bootingDevices": [{ "hostId": "local", "id": "b", "platform": "ios", "name": "iPad", "version": "18", "booted": false, "physical": false, "threadId": "t" }],
        "somethingNew": true,
    }));
    assert_eq!(decoded.hosts.iter().map(|host| (host.id.as_str(), host.kind)).collect::<Vec<_>>(), [("local", HostKind::Local), ("eas", HostKind::Eas), ("lab", HostKind::Simlock)]);
    assert_eq!(decoded.devices.len(), 1, "an unknown platform or a broken device is dropped, not the state");
    assert_eq!(decoded.host_statuses["eas"].status, HostStatus::Unknown);
    assert!(!decoded.host_statuses.contains_key("bad"));
    assert_eq!(decoded.booting_names("t"), ["iPad"]);
    assert!(!decoded.supports_eas_hosts && !decoded.supports_simlock_hosts, "older servers send no capability");
    assert!(state(json!({ "supportsEasHosts": true, "supportsSimlockHosts": true })).supports_eas_hosts);
    assert!(ServiceState::decode(json!({ "hostStatus": "ready" })).is_err());
}

#[test]
fn readiness_counts_remote_hosts_and_setup_follows_onboarding_and_the_hub() {
    let idle_with_ready_remote = state(json!({ "hostStatus": "idle", "hostStatuses": { "mac": { "status": "ready" } } }));
    assert_eq!(idle_with_ready_remote.hub_readiness(), Readiness { ready: true, busy: false });
    let installing = state(json!({ "hostStatus": "idle", "hostStatuses": { "mac": { "status": "installing" } } }));
    assert_eq!(installing.hub_readiness(), Readiness { ready: false, busy: true });
    assert!(!state(json!({})).needs_setup());
    assert!(state(json!({ "onboardingCompleted": false })).needs_setup());
    assert!(state(json!({ "hostStatus": "disabled" })).needs_setup());
    assert!(state(json!({ "hostStatus": "disabled" })).host_updates().is_empty());
}

#[test]
fn platform_setup_status_explains_each_platform_in_the_web_words() {
    let local = state(json!({ "devices": [device("local", "a", "ios", "iPhone", false)] }));
    assert_eq!(local.platform_setup_status(Platform::Ios), PlatformStatus { ready: true, message: "Xcode and iOS Simulator are available.".into() });
    assert_eq!(local.platform_setup_status(Platform::Android), PlatformStatus { ready: false, message: "No Android SDK.".into() });
    let no_simulators = state(json!({}));
    assert!(no_simulators.platform_setup_status(Platform::Ios).message.starts_with("Xcode is installed, but no iOS Simulator"));
    let remote = state(json!({
        "hostStatus": "idle",
        "hosts": [host("local", "local", json!([])), host("mac", "ssh", json!([{ "platform": "ios", "available": true }]))],
        "hostStatuses": { "mac": { "status": "starting" } },
    }));
    assert_eq!(remote.platform_setup_status(Platform::Ios).message, "Xcode and iOS Simulator are available on mac host.");
    assert_eq!(remote.platform_setup_status(Platform::Android).message, "Android support was not detected.");
    assert!(remote.local_platforms_unavailable());
    let eas = state(json!({ "hosts": [host("eas", "eas", json!([{ "platform": "android", "available": true }]))] }));
    assert_eq!(eas.platform_setup_status(Platform::Android).message, "eas host starts Android emulators in the cloud with Expo Simulators.");
    let uncertain = state(json!({ "hostStatuses": { "local": { "status": "ready", "androidDiscoveryIncomplete": true } } }));
    assert!(uncertain.platform_setup_status(Platform::Android).message.starts_with("The Android device list may be incomplete"));
    assert!(!uncertain.show_android_hint());
}

#[test]
fn the_panel_lists_running_devices_first_and_explains_what_is_missing() {
    let listed = state(json!({
        "hosts": [host("local", "local", json!([{ "platform": "ios", "available": true }, { "platform": "android", "available": true }]))],
        "devices": [device("local", "b", "ios", "iPhone b", false), device("local", "a", "ios", "iPhone A", false), device("local", "z", "ios", "iPhone Z", true)],
    }));
    let grouped = listed.grouped();
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].1.iter().map(|device| device.name.as_str()).collect::<Vec<_>>(), ["iPhone Z", "iPhone A", "iPhone b"]);
    assert_eq!(listed.device_detail(grouped[0].1[0]), "local host · 18.2 · Running");
    assert_eq!(listed.device_description(grouped[0].1[1]), "local host · 18.2");
    assert!(listed.show_android_hint(), "Android is available but has no emulator");
    // A host that rules Android out says why in setup instead.
    assert!(!state(json!({})).show_android_hint());
    assert_eq!(state(json!({})).empty_message(), "No simulators or emulators were found on this environment.");
    assert_eq!(state(json!({ "hostStatus": "failed", "hostStatusDetail": "xcrun missing" })).empty_message(), "xcrun missing");
    let booted = &listed.devices[2];
    assert_eq!(listed.loading_message(Some(booted)), "Opening device…");
    assert_eq!(listed.loading_message(Some(&listed.devices[0])), "Starting device…");
    assert_eq!(state(json!({ "hostStatus": "installing" })).loading_message(None), "Installing device support…");
    assert_eq!(listed.loading_message(None), "Finding devices…");
}

#[test]
fn warnings_and_host_updates_follow_each_host_status() {
    let warned = state(json!({
        "hostStatusDetail": "Simulator list is slow",
        "hosts": [host("local", "local", json!([])), host("mac", "ssh", json!([])), host("pi", "ssh", json!([]))],
        "hostStatuses": { "mac": { "status": "failed", "detail": "ssh refused" }, "pi": { "status": "installing" } },
        "supportsHostRetry": true,
    }));
    assert_eq!(warned.panel_diagnostics(), [Diagnostic { host_id: "local".into(), label: "local host".into(), detail: "Simulator list is slow".into() }]);
    assert!(warned.setup_diagnostics().is_empty(), "setup reads only per-host details");
    let updates = warned.host_updates();
    assert_eq!(updates.len(), 2);
    assert_eq!((updates[0].detail.as_str(), updates[0].failed, updates[0].retry), ("ssh refused", true, true));
    assert_eq!((updates[1].detail.as_str(), updates[1].failed, updates[1].retry), ("Installing device tools…", false, false));
}

#[test]
fn setup_statuses_read_as_the_wizard_shows_them() {
    let ready = state(json!({ "agentAccessEnabled": true, "hosts": [{ "id": "local", "label": "This machine", "kind": "local", "platforms": [], "hubInstalled": true, "agentDeviceInstalled": true }] }));
    assert_eq!(ready.hub_setup_status(false), Some(SetupStatus { pending: false, text: "Device hub is ready." }));
    assert_eq!(ready.agent_setup_status(false), Some(SetupStatus { pending: false, text: "Agent tools are ready." }));
    let disabled = state(json!({ "hostStatus": "disabled" }));
    assert_eq!(disabled.hub_setup_status(false), None);
    assert_eq!(disabled.hub_setup_status(true).map(|status| status.text), Some("Updating device hub…"));
    assert_eq!(state(json!({ "hostStatus": "installing" })).hub_setup_status(true).map(|status| status.text), Some("Installing device hub…"));
    assert_eq!(state(json!({ "hostStatus": "starting" })).agent_setup_status(true).map(|status| status.text), Some("Starting agent tools…"));
    assert_eq!(configure_hub_payload(false), json!({ "enabled": false, "agentAccessEnabled": false }));
    assert_eq!(configure_hub_payload(true), json!({ "enabled": true }));
}

#[test]
fn payloads_and_hub_urls_match_the_web_client() {
    let decoded = state(json!({ "devices": [device("mac", "A B", "ios", "iPhone", true)] }));
    assert_eq!(open_payload("t", &decoded.devices[0]), json!({ "threadId": "t", "hostId": "mac", "deviceId": "A B", "platform": "ios" }));
    assert_eq!(close_payload("t", "mac", "A B", true), json!({ "threadId": "t", "hostId": "mac", "deviceId": "A B", "shutdown": true }));
    assert_eq!(Opened::decode(json!({ "threadId": "t", "hostId": "mac", "deviceId": "A B", "platform": "ios", "openedAt": "now" })).unwrap().device_id, "A B");
    let access = HubAccess::new("https://host.example:3773/base?x=1", "tick et", "mac").unwrap();
    let device = encode_component("A B");
    assert_eq!(
        access.http_url(true, &format!("/helper/{device}/stream.mjpeg")),
        "https://host.example:3773/api/device-hub/vendor/serve-sim/helper/A%20B/stream.mjpeg?wsTicket=tick+et&hostId=mac"
    );
    assert_eq!(access.ws_url(true, &format!("/helper/ws?device={device}")), "wss://host.example:3773/api/device-hub/vendor/serve-sim/helper/ws?device=A%20B&wsTicket=tick+et&hostId=mac");
    assert_eq!(encode_component("a/b!~'()*"), "a%2Fb!~'()*");
}

#[test]
fn ios_input_frames_match_serve_sim() {
    let portrait = Screen { width: 1170., height: 2532., orientation: Orientation::Portrait, input_unavailable: false };
    let touch_frame = touch(TouchPhase::Begin, 0.25, 0.5, Some(&portrait));
    assert_eq!(touch_frame[0], 0x03);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&touch_frame[1..]).unwrap(), json!({ "type": "begin", "x": 0.25, "y": 0.5 }));
    // A landscape screen still streams the raw portrait framebuffer; input is remapped into it.
    let left = Screen { orientation: Orientation::LandscapeLeft, ..portrait };
    assert_eq!(raw_point(Some(&left), 0.25, 0.5), (0.5, 0.75));
    assert_eq!(raw_point(Some(&Screen { orientation: Orientation::LandscapeRight, ..portrait }), 0.25, 0.5), (0.5, 0.25));
    assert_eq!(raw_point(Some(&Screen { orientation: Orientation::PortraitUpsideDown, ..portrait }), 0.25, 0.5), (0.75, 0.5));
    // A framebuffer that is already landscape needs no remap.
    assert_eq!(raw_point(Some(&Screen { width: 2532., height: 1170., ..left }), 0.25, 0.5), (0.25, 0.5));
    assert_eq!(button(HardwareButton::Home), [&[0x04][..], br#"{"button":"home"}"#].concat());
    assert_eq!(button(HardwareButton::AppSwitcher), [&[0x04][..], br#"{"button":"app_switcher"}"#].concat());
    assert_eq!(key(KeyPhase::Down, "KeyA"), Some([&[0x06][..], br#"{"type":"down","usage":4}"#].concat()));
    assert_eq!(key(KeyPhase::Up, "Digit0"), Some([&[0x06][..], br#"{"type":"up","usage":39}"#].concat()));
    assert_eq!(key(KeyPhase::Down, "F5"), None);
    assert_eq!((hid_usage("Digit1"), hid_usage("KeyZ"), hid_usage("ShiftLeft"), hid_usage("Keya")), (Some(0x1e), Some(0x1d), Some(0xe1), None));
    assert_eq!(orientation(Orientation::LandscapeRight), [&[0x07][..], br#"{"orientation":"landscape_right"}"#].concat());
    let cycle: Vec<Orientation> = std::iter::successors(Some(Orientation::Portrait), |o| Some(o.next())).take(5).collect();
    assert_eq!(cycle, [Orientation::Portrait, Orientation::LandscapeLeft, Orientation::PortraitUpsideDown, Orientation::LandscapeRight, Orientation::Portrait]);
}

#[test]
fn ios_socket_messages_decode_and_closes_classify() {
    assert_eq!(decode_ios_message(&[0x83]), IosMessage::InputAdmitted);
    let config = [&[0x82][..], br#"{"width":1170,"height":2532,"orientation":"landscape_left","inputUnavailable":true,"screenId":1}"#].concat();
    assert_eq!(decode_ios_message(&config), IosMessage::Screen(Screen { width: 1170., height: 2532., orientation: Orientation::LandscapeLeft, input_unavailable: true }));
    assert_eq!(decode_ios_message(&[&[0x82][..], br#"{"width":0,"height":2532,"orientation":"portrait"}"#].concat()), IosMessage::Other);
    assert_eq!(decode_ios_message(&[&[0x82][..], br#"{"width":1,"height":2,"orientation":"sideways"}"#].concat()), IosMessage::Other);
    assert_eq!(decode_ios_message(&[0x90, b'{', b'}']), IosMessage::Other);
    assert_eq!(decode_ios_message(&[]), IosMessage::Other);
    assert_eq!((classify_close(Some(4401)), classify_close(Some(1008)), classify_close(Some(1006)), classify_close(None)), (CloseAction::Unauthorized, CloseAction::Unauthorized, CloseAction::Retry, CloseAction::Retry));
    // An ended Simlock lease stops instead of retrying, and keeps the gateway's reason.
    assert_eq!(classify_close(Some(LEASE_ENDED_CLOSE)), CloseAction::LeaseEnded);
    assert_eq!((lease_ended("expired"), lease_ended("")), ("Device lease ended: expired".to_owned(), "Device lease ended: ended".to_owned()));
    assert_eq!((close_detail(Some(1011), ""), close_detail(Some(1006), ""), close_detail(None, "gone")), ("closed 1011".to_owned(), "input socket refused".to_owned(), "gone".to_owned()));
    assert_eq!(input_disconnected_label(Some("closed 1011")), "Input disconnected (closed 1011), reconnecting…");
    assert_eq!(input_disconnected_label(None), "Input disconnected, reconnecting…");
}

#[test]
fn transports_follow_platform_and_host() {
    assert_eq!(transport(Platform::Ios, Some(HostKind::Local)), Ok(Transport::Mjpeg));
    assert_eq!(transport(Platform::Ios, None), Ok(Transport::Mjpeg));
    assert!(transport(Platform::Android, Some(HostKind::Local)).unwrap_err().contains("Android stream"));
    assert!(transport(Platform::Ios, Some(HostKind::Simlock)).unwrap_err().contains("this device's stream"));
}

#[test]
fn the_frame_fits_its_aspect_and_rotates_raw_portrait_frames() {
    let portrait = Screen { width: 1170., height: 2532., orientation: Orientation::Portrait, input_unavailable: false };
    let left = Screen { orientation: Orientation::LandscapeLeft, ..portrait };
    assert!((screen_aspect(Platform::Ios, Some(&portrait)) - 1170. / 2532.).abs() < 1e-9);
    assert!((screen_aspect(Platform::Ios, Some(&left)) - 2532. / 1170.).abs() < 1e-9);
    assert!((screen_aspect(Platform::Ios, None) - 9. / 19.5).abs() < 1e-9);
    assert_eq!((screen_rotation(Platform::Ios, Some(&left)), screen_rotation(Platform::Ios, Some(&portrait))), (90, 0));
    assert_eq!(screen_rotation(Platform::Ios, Some(&Screen { orientation: Orientation::LandscapeRight, ..portrait })), -90);
    assert_eq!(screen_rotation(Platform::Ios, Some(&Screen { width: 2532., height: 1170., ..left })), 0);
    // Height-bound when the width allows it, else width-bound; the rail's inset comes off the width.
    assert_eq!(fit_frame(0.5, 400., 600., 56.), (300., 600.));
    assert_eq!(fit_frame(2., 456., 600., 56.), (400., 200.));
    assert_eq!(fit_frame(0.5, 56., 600., 56.), (0., 0.));
}

#[test]
fn mjpeg_parts_parse_across_chunks_and_only_the_newest_is_kept() {
    let part = |body: &[u8]| [format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(), body, b"\r\n"].concat();
    let mut parser = MjpegParser::new(Some("multipart/x-mixed-replace; boundary=frame"));
    let stream = [part(b"\xff\xd8one\xff\xd9"), part(b"\xff\xd8two\r\n--frame\xff\xd9"), part(b"\xff\xd8three\xff\xd9")].concat();
    // Byte by byte: every part completes once its last body byte arrives.
    let mut seen = Vec::new();
    for byte in &stream {
        if let Some(frame) = parser.push(std::slice::from_ref(byte)) {
            seen.push(frame);
        }
    }
    assert_eq!(seen, [b"\xff\xd8one\xff\xd9".to_vec(), b"\xff\xd8two\r\n--frame\xff\xd9".to_vec(), b"\xff\xd8three\xff\xd9".to_vec()]);
    // All at once: only the newest comes back.
    let mut parser = MjpegParser::new(Some("multipart/x-mixed-replace;boundary=\"frame\""));
    assert_eq!(parser.push(&stream), Some(b"\xff\xd8three\xff\xd9".to_vec()));
    assert_eq!(parser.push(b""), None);
    // Without lengths a part runs to the next boundary; a preamble is skipped.
    let mut parser = MjpegParser::new(Some("multipart/x-mixed-replace; boundary=--edge"));
    assert_eq!(parser.push(b"junk\r\n--edge\r\nContent-Type: image/jpeg\r\n\r\nAAA\r\n--edge\r\n\r\nBB"), Some(b"AAA".to_vec()));
    assert_eq!(parser.push(b"B\r\n--edge"), Some(b"BBB".to_vec()));
    // An oversized length is broken framing: that part is skipped, the next one still parses.
    let mut parser = MjpegParser::new(None);
    let broken = format!("--frame\r\nContent-Length: {}\r\n\r\n", 64 * 1024 * 1024);
    assert_eq!(parser.push(&[broken.as_bytes(), &part(b"ok")].concat()), Some(b"ok".to_vec()));
}

#[test]
fn a_restarted_server_replaces_newer_revisions_and_failures_matter_only_before_a_first_state() {
    let mut feed = DeviceFeed::default();
    assert!(feed.failed("socket closed".into()));
    assert_eq!(feed.load_error(), Some("socket closed"));
    assert!(feed.retry());
    let before = state(json!({ "revision": 7, "devices": [device("local", "a", "ios", "iPhone", true)], "sessions": [{ "threadId": "t", "hostId": "local", "deviceId": "a", "platform": "ios", "openedAt": "x" }] }));
    assert!(feed.snapshot(before.clone()));
    assert!(!feed.snapshot(before), "an unchanged snapshot changes nothing");
    // After a server restart the resubscribed feed starts again at revision 0 and wins.
    let restarted = state(json!({ "revision": 0 }));
    assert!(feed.snapshot(restarted));
    assert_eq!(feed.service().map(|service| (service.revision, service.devices.len(), service.sessions.len())), Some((0, 0, 0)));
    assert!(!feed.failed("socket closed".into()), "a dropped feed keeps the last state");
    assert_eq!(feed.load_error(), None);
}

#[test]
fn operations_from_an_earlier_connection_never_apply() {
    let decoded = state(json!({ "devices": [device("local", "a", "ios", "iPhone", false)] }));
    let mut operations = Operations::default();
    let opening = operations.start("t", Some(decoded.devices[0].clone()));
    assert!(operations.get("t").is_some_and(|operation| operation.pending.is_some()));
    // The environment changed while the open was in flight.
    operations.reset();
    assert!(!operations.is_current(opening));
    assert!(!operations.settle("t", opening, Some("late".into())));
    assert!(operations.get("t").is_none());
    // A newer operation on the thread is not settled by an older one.
    let first = operations.start("t", None);
    let second = operations.start("t", None);
    assert!(!operations.settle("t", first, Some("superseded".into())));
    assert!(operations.settle("t", second, Some("refused".into())));
    assert_eq!(operations.get("t").and_then(|operation| operation.error.as_deref()), Some("refused"));
    assert!(operations.dismiss("t"));
}

#[test]
fn the_probe_runs_each_time_it_becomes_active_including_when_state_arrives_late() {
    let mut gate = ProbeGate::default();
    // Visible before the first state: nothing to probe yet.
    assert!(!gate.update(false));
    // The first state arrives while visible: probe once.
    assert!(gate.update(true));
    assert!(!gate.update(true));
    // Hidden, then shown again: probe again.
    assert!(!gate.update(false));
    assert!(gate.update(true));
}

fn detail(settings: serde_json::Value) -> tools::Detail {
    tools::Detail::decode(json!({ "hostId": "local", "deviceId": "sim", "settings": settings, "foregroundApp": { "id": "com.example" }, "readAt": "now" })).unwrap()
}

#[test]
fn device_detail_decodes_leniently_and_actions_encode_as_the_contract_does() {
    let decoded = detail(json!({ "appearance": "dark", "textSize": "extra-large", "reduceMotion": true, "colorFilter": "sepia", "liquidGlass": 3, "somethingNew": 1 }));
    assert_eq!(decoded.settings.appearance, Some(tools::Appearance::Dark));
    assert_eq!(decoded.settings.text_size, Some(tools::TextSize::ExtraLarge));
    assert_eq!(decoded.settings.reduce_motion, Some(true));
    assert_eq!((decoded.settings.color_filter, decoded.settings.liquid_glass), (None, None), "a value a newer host adds reads as unknown");
    assert_eq!(decoded.foreground_app.as_ref().map(|app| app.id.as_str()), Some("com.example"));
    let no_app = tools::Detail::decode(json!({ "hostId": "local", "deviceId": "sim", "settings": {}, "foregroundApp": null, "readAt": "now" })).unwrap();
    assert_eq!(no_app.foreground_app, None);
    assert_eq!(tools::detail_payload("mac", "sim"), json!({ "hostId": "mac", "deviceId": "sim" }));
    let payload = |action: tools::Action| tools::action_payload("mac", "sim", &action);
    assert_eq!(payload(tools::Action::SetAppearance(tools::Appearance::Light)), json!({ "hostId": "mac", "deviceId": "sim", "type": "setAppearance", "value": "light" }));
    assert_eq!(payload(tools::Action::SetTextSize(tools::TextSize::ExtraLarge))["value"], "extra-large");
    assert_eq!(payload(tools::Action::SetToggle(tools::Toggle::NetworkEnabled, false)), json!({ "hostId": "mac", "deviceId": "sim", "type": "setToggle", "setting": "networkEnabled", "value": false }));
    assert_eq!(payload(tools::Action::SetOrientation(Orientation::LandscapeLeft))["value"], "landscape_left");
    assert_eq!(payload(tools::Action::SetColorFilter(tools::ColorFilter::RedGreen))["value"], "red-green");
    assert_eq!(payload(tools::Action::SetLocation { latitude: 59.3293, longitude: 18.0686 }), json!({ "hostId": "mac", "deviceId": "sim", "type": "setLocation", "latitude": 59.3293, "longitude": 18.0686 }));
    assert_eq!(payload(tools::Action::ClearLocation), json!({ "hostId": "mac", "deviceId": "sim", "type": "clearLocation" }));
    assert_eq!(
        payload(tools::Action::SetPermission { app_id: "com.example".into(), permission: tools::Permission::MediaLibrary, decision: tools::Decision::Reset }),
        json!({ "hostId": "mac", "deviceId": "sim", "type": "setPermission", "appId": "com.example", "permission": "media-library", "decision": "reset" })
    );
    assert_eq!(payload(tools::Action::OpenUrl("myapp://home".into())), json!({ "hostId": "mac", "deviceId": "sim", "type": "openUrl", "url": "myapp://home" }));
    assert_eq!(payload(tools::Action::TerminateApp("com.example".into()))["type"], "terminateApp");
    assert_eq!(payload(tools::Action::SendPush { app_id: "com.example".into(), payload: "Hi".into() })["payload"], "Hi");
}

#[test]
fn device_controls_serialize_actions_and_drop_reads_an_action_overtook() {
    let mut controls = tools::Controls::default();
    assert!(!controls.begin(true), "nothing runs before the first detail");
    assert!(controls.disabled(true));
    let first = controls.start_read();
    assert!(controls.read(first, Ok(detail(json!({ "appearance": "light" })))));
    assert!(!controls.disabled(true));
    assert!(controls.disabled(false), "a hidden device takes no action");
    assert!(!controls.begin(false));
    // A read sent before an action must not undo what the action confirms.
    let stale = controls.start_read();
    assert!(controls.begin(true));
    assert!(controls.pending() && controls.disabled(true));
    assert!(!controls.begin(true), "one action at a time");
    assert!(controls.finish(Ok(detail(json!({ "appearance": "dark" })))));
    assert!(!controls.read(stale, Ok(detail(json!({ "appearance": "light" })))));
    assert_eq!(controls.settings().and_then(|settings| settings.appearance), Some(tools::Appearance::Dark));
    // A failed action keeps the last detail and says why; the next action clears the error.
    assert!(controls.begin(true));
    assert!(!controls.finish(Err("Device setAppearance failed".into())));
    assert_eq!((controls.error(), controls.pending()), (Some("Device setAppearance failed"), false));
    assert!(controls.detail().is_some());
    assert!(controls.begin(true));
    assert_eq!(controls.error(), None);
    // A failed read clears the detail, as `deviceDetailReadAtom` does.
    let mut reread = tools::Controls::default();
    let ticket = reread.start_read();
    assert!(reread.read(ticket, Err("Device host is unavailable".into())));
    assert_eq!((reread.detail(), reread.error()), (None, Some("Device host is unavailable")));
}

#[test]
fn device_rail_and_tools_rules_follow_the_web() {
    assert!(tools::runs_host_commands(Some(HostKind::Local)) && tools::runs_host_commands(Some(HostKind::Ssh)));
    assert!(!tools::runs_host_commands(Some(HostKind::Eas)) && !tools::runs_host_commands(Some(HostKind::Simlock)) && !tools::runs_host_commands(None));
    assert!(!tools::offers_screenshot(Some(HostKind::Simlock)) && tools::offers_screenshot(Some(HostKind::Eas)));
    let dark = detail(json!({ "appearance": "dark" })).settings;
    assert_eq!(tools::next_appearance(Some(&dark)), tools::Appearance::Light);
    assert_eq!(tools::next_appearance(None), tools::Appearance::Dark);
    assert_eq!(tools::appearance_label(tools::Appearance::Light), "Switch device to light mode");
    assert_eq!(tools::Toggle::for_platform(Platform::Android), [tools::Toggle::ReduceMotion, tools::Toggle::NetworkEnabled]);
    assert_eq!(tools::Permission::for_platform(Platform::Android).len(), 8);
    assert_eq!(tools::Permission::parse("faceid"), Some(tools::Permission::FaceId));
    assert_eq!(tools::parse_orientation("portrait_upside_down"), Some(Orientation::PortraitUpsideDown));
    assert_eq!(tools::TextSize::parse("large"), Some(tools::TextSize::Large));
    assert_eq!(tools::parse_location(" 59.3 ", "18"), Some((59.3, 18.)));
    assert_eq!((tools::parse_location("91", "0"), tools::parse_location("0", "-181"), tools::parse_location("", "0"), tools::parse_location("x", "0")), (None, None, None, None));
    assert_eq!((tools::resolve_app_id("  ", Some("com.fg")), tools::resolve_app_id(" com.typed ", Some("com.fg")), tools::resolve_app_id("", None)), (Some("com.fg"), Some("com.typed"), None));
    assert_eq!((tools::submit_value("  https://x.dev "), tools::submit_value("   ")), (Some("https://x.dev"), None));
}

#[test]
fn device_screenshot_extracts_the_png_and_names_the_file() {
    // A minimal PNG: signature, one IHDR-sized chunk, IEND; `adb` may print warnings around it.
    let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10];
    png.extend([0, 0, 0, 1]);
    png.extend(b"IHDR");
    png.push(7);
    png.extend([0, 0, 0, 0]);
    png.extend([0, 0, 0, 0]);
    png.extend(b"IEND");
    png.extend([0, 0, 0, 0]);
    let noisy = [b"WARNING: multiple displays\n".as_slice(), &png, b"trailing"].concat();
    assert_eq!(tools::extract_png(&noisy), Some(png.as_slice()));
    assert_eq!(tools::extract_png(&png[..png.len() - 3]), None, "a truncated image is incomplete");
    assert_eq!(tools::extract_png(b"no image"), None);
    assert_eq!(tools::screenshot_file_name("iPhone 16 Pro (18.2)", 1_700), "iPhone-16-Pro--18-2--1700.png");
    assert_eq!(tools::screenshot_path("SIM 1"), "/api/screenshot?device=SIM%201");
    assert_eq!(tools::screenshot_status_error(500), "Screenshot capture failed (500). Try again.");
}

#[test]
fn eas_host_access_token_is_a_literal_or_a_secret_reference() {
    let base = json!({ "kind": "eas", "id": "expo", "label": " Expo " });
    let host = hosts::EasHostConfig::decode(&base).unwrap();
    assert_eq!((host.label.as_str(), &host.access_token, host.max_session_minutes, host.idle_minutes, host.max_concurrent_sessions), ("Expo", &hosts::SecretValue::default(), 30, 10, 2));
    let with = |patch: serde_json::Value| {
        let mut value = base.clone();
        for (key, field) in patch.as_object().unwrap() {
            value[key] = field.clone();
        }
        hosts::EasHostConfig::decode(&value)
    };
    let referenced = with(json!({ "accessToken": { "provider": "1Password", "reference": " op://vault/expo/token " } })).unwrap();
    assert_eq!(referenced.access_token, hosts::SecretValue::Reference { provider: "1Password".into(), reference: "op://vault/expo/token".into() });
    assert_eq!(referenced.access_token.literal(), "", "a reference holds no literal");
    assert_eq!(hosts::EasHostConfig::decode(&referenced.encode()).unwrap(), referenced, "it round-trips");
    assert_eq!(with(json!({ "accessToken": " tok " })).unwrap().access_token.literal(), "tok");
    assert_eq!(with(json!({ "accessToken": { "provider": "-bad", "reference": "x" } })), Err(hosts::REFERENCE_PROBLEM.into()));
    assert_eq!(with(json!({ "accessToken": { "provider": "op", "reference": "a\nb" } })), Err(hosts::REFERENCE_PROBLEM.into()));
    assert_eq!(with(json!({ "accessToken": 4 })), Err(hosts::REFERENCE_PROBLEM.into()));
    assert_eq!(with(json!({ "idleMinutes": 30 })), Err(hosts::IDLE_LIMIT_PROBLEM.into()));
    assert!(with(json!({ "maxSessionMinutes": 116 })).is_err());
    assert!(with(json!({ "maxConcurrentSessions": 2.5 })).is_err());
    assert!(with(json!({ "id": "local" })).is_err(), "the local id is reserved");
    assert!(with(json!({ "id": "_x" })).is_err());
    assert!(with(json!({ "kind": "ssh" })).is_err());
    assert!(hosts::decode_eas_hosts(&json!([base.clone(), base.clone()])).is_err(), "ids are unique");
    assert_eq!(host.session_limits(), "Up to 2 sessions at once, each stopped after 30 min or 10 idle min");
    let updated = hosts::update_eas_hosts(std::slice::from_ref(&referenced), &hosts::EasHostConfig { label: "Renamed".into(), ..hosts::EasHostConfig::new("expo") }, false, true);
    assert_eq!((updated[0].label.as_str(), &updated[0].access_token), ("Renamed", &referenced.access_token), "keepToken keeps the environment's token");
    assert!(hosts::update_eas_hosts(&updated, &host, true, false).is_empty());
}

#[test]
fn eas_token_editor_blocks_check_and_save_on_an_invalid_reference_until_cleared() {
    let saved = hosts::EasHostConfig { access_token: hosts::SecretValue::Literal("•••".into()), ..hosts::EasHostConfig::new("expo") };
    let mut editor = hosts::EasTokenEditor::new(&saved, false);
    assert!(editor.configured() && editor.field_value().is_empty(), "a saved literal arrives redacted");
    assert_eq!(editor.open_check_payload("expo"), json!({ "hostId": "expo" }));
    assert_eq!(editor.check_payload("expo"), json!({ "hostId": "expo" }));
    assert_eq!(editor.token_for_save(), saved.access_token);
    // Picking a provider before its reference is committed is invalid.
    editor.set_validity(false);
    assert!(!editor.can_check());
    assert_eq!(editor.problem(&saved), Some(hosts::REFERENCE_PROBLEM));
    let reference = hosts::SecretValue::reference("op", "op://vault/expo").unwrap();
    editor.set_token(Some(reference.clone()));
    editor.set_validity(true);
    assert_eq!(editor.problem(&saved), None);
    assert_eq!(editor.check_payload("expo"), json!({ "accessToken": { "provider": "op", "reference": "op://vault/expo" } }));
    assert_eq!(editor.next(&saved).access_token, reference);
    // Clear resets validation and sends an empty token, so the check cannot fall back to the saved one.
    editor.set_validity(false);
    editor.clear();
    assert!(editor.valid() && editor.can_check());
    assert_eq!(editor.check_payload("expo"), json!({ "accessToken": "" }));
    assert!(editor.token_for_save().is_empty());
    let idle = hosts::EasHostConfig { idle_minutes: 40, max_session_minutes: 40, ..saved.clone() };
    assert_eq!(editor.problem(&idle), Some(hosts::IDLE_LIMIT_PROBLEM));
    assert_eq!(editor.problem(&hosts::EasHostConfig { label: " ".into(), ..saved.clone() }), Some(hosts::LIMITS_PROBLEM));
    // A saved reference is shown as written; a new host's saved token checks without an id.
    let referenced = hosts::EasHostConfig { access_token: reference.clone(), ..saved.clone() };
    let editor = hosts::EasTokenEditor::new(&referenced, true);
    assert!(!editor.configured());
    assert_eq!(editor.field_value(), reference);
    assert_eq!((editor.open_check_payload("expo"), editor.check_payload("expo")), (json!({}), json!({})));
    let empty = hosts::EasTokenEditor::new(&hosts::EasHostConfig::new("expo"), false);
    assert_eq!((empty.token(), empty.open_check_payload("expo")), (&hosts::TokenDraft::Cleared, json!({ "accessToken": "" })));
    let account = hosts::EasAccount::decode(json!({ "signedInAs": "ana", "credentialSource": "eas-login", "apps": [{ "id": "a1", "fullName": "@ana/app" }] })).unwrap();
    assert_eq!((account.credential_source_label(), account.apps[0].full_name.as_str()), ("the server's eas login", "@ana/app"));
}

#[test]
fn ssh_simlock_and_tool_versions_decode_validate_and_patch() {
    let decoded = state(json!({
        "supportsToolUpdate": true,
        "supportsToolInspection": true,
        "hosts": [{
            "id": "local",
            "label": "This machine",
            "kind": "local",
            "platforms": [],
            "agentDeviceInstalled": true,
            "tools": {
                "hub": { "requiredVersion": "0.5.0", "installedVersions": ["0.4.0"], "runningVersion": "0.4.0" },
                "agent": { "requiredVersion": "1.2.0", "installedVersions": ["1.2.0"], "runningVersion": null }
            }
        }]
    }));
    assert!(decoded.supports_tool_update && decoded.supports_tool_inspection);
    let tools = decoded.host("local").unwrap().tools.as_ref().unwrap();
    assert_eq!(tools.hub.display_version(), Some("0.4.0"));
    assert!(tools.hub.needs_update());
    assert_eq!(tools.agent.display_version(), Some("1.2.0"));
    assert!(!tools.agent.needs_update());
    let numeric = ToolVersion {
        required_version: "11.0.0".into(),
        installed_versions: vec!["9.0.0".into(), "10.0.0".into(), "2.1.0".into()],
        running_version: None,
    };
    assert_eq!(numeric.display_version(), Some("10.0.0"));
    assert_eq!(update_tool_payload("hub"), json!({ "updateTool": "hub" }));
    assert_eq!(inspect_only_payload(), json!({ "inspectOnly": true }));

    // SSH hosts
    let ssh_list = hosts::decode_ssh_hosts(&json!([
        { "id": "mac", "label": "Mac mini", "target": "dev@mac.local", "port": 2222, "identityFile": "~/.ssh/id_ed25519" }
    ]))
    .unwrap();
    assert_eq!(ssh_list.len(), 1);
    assert_eq!(ssh_list[0].port, Some(2222));
    assert!(hosts::SshHostConfig { target: "-oBad".into(), ..ssh_list[0].clone() }.validate().is_err());
    let defaulted_label = hosts::SshHostConfig { label: "  ".into(), ..ssh_list[0].clone() }.validate().unwrap();
    assert_eq!(defaulted_label.label, "dev@mac.local");
    let updated_ssh = hosts::update_ssh_hosts(&ssh_list, &defaulted_label, false);
    assert_eq!(updated_ssh.len(), 1);
    assert_eq!(hosts::update_ssh_hosts(&ssh_list, &defaulted_label, true).len(), 0);

    // Simlock hosts & token editor
    let simlock_list = hosts::decode_simlock_hosts(&json!([
        {
            "kind": "simlock",
            "id": "pool-1",
            "label": "Office pool",
            "gatewayUrl": "https://simlock.corp.ts.net:4700",
            "agentToken": { "provider": "op", "reference": "op://Dev/Simlock/token" },
            "maxLeases": 4,
            "maxLeaseMinutes": 90
        }
    ]))
    .unwrap();
    assert_eq!(simlock_list.len(), 1);
    assert_eq!(simlock_list[0].lease_limits(), "Up to 4 leases at once, each up to 90 min");
    let mut sim_ed = hosts::SimlockTokenEditor::new(&simlock_list[0], false);
    assert!(!sim_ed.configured());
    assert_eq!(sim_ed.problem(&simlock_list[0]), None);
    assert_eq!(
        sim_ed.check_payload("pool-1", "https://simlock.corp.ts.net:4700/"),
        json!({ "gatewayUrl": "https://simlock.corp.ts.net:4700", "hostId": "pool-1" })
    );
    sim_ed.set_validity(false);
    assert_eq!(sim_ed.problem(&simlock_list[0]), Some(hosts::REFERENCE_PROBLEM));
    sim_ed.clear();
    assert_eq!(
        sim_ed.check_payload("pool-1", "https://simlock.corp.ts.net:4700"),
        json!({ "gatewayUrl": "https://simlock.corp.ts.net:4700", "agentToken": "" })
    );
    let bad_gw = hosts::SimlockHostConfig { gateway_url: "ftp://bad".into(), ..simlock_list[0].clone() };
    assert_eq!(sim_ed.problem(&bad_gw), Some(hosts::SIMLOCK_GATEWAY_PROBLEM));
    let ident = hosts::SimlockIdentity::decode(json!({
        "gatewayUrl": "https://simlock.corp.ts.net:4700",
        "signedInAs": "ci-agent",
        "tokenSource": "env"
    }))
    .unwrap();
    assert_eq!(ident.token_source_label(), "AGENT_DEVICE_DAEMON_AUTH_TOKEN on the server");
}

#[test]
fn agent_session_tracker_opens_new_sessions_after_baseline_and_waits_for_device() {
    let mut tracker = AgentSessionTracker::default();
    let mut state = ServiceState::empty();
    let iphone = Device {
        host_id: "local".into(),
        id: "sim-1".into(),
        platform: Platform::Ios,
        name: "iPhone 16".into(),
        version: "iOS 18.0".into(),
        booted: true,
    };
    let pixel = Device {
        host_id: "local".into(),
        id: "emu-1".into(),
        platform: Platform::Android,
        name: "Pixel 9".into(),
        version: "API 35".into(),
        booted: true,
    };
    state.devices.push(iphone.clone());
    state.sessions.push(Session {
        thread_id: "t-1".into(),
        host_id: "local".into(),
        device_id: "sim-1".into(),
        platform: Platform::Ios,
    });
    // First snapshot sets baseline only.
    assert!(tracker.apply(&state).is_empty());

    // Agent opens a session before its Device summary arrives: stays out of baseline.
    state.sessions.push(Session {
        thread_id: "t-2".into(),
        host_id: "local".into(),
        device_id: "emu-1".into(),
        platform: Platform::Android,
    });
    assert!(tracker.apply(&state).is_empty());

    // Once the Device summary lands, the session opens.
    state.devices.push(pixel);
    let opened = tracker.apply(&state);
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].thread_id, "t-2");
    assert_eq!(opened[0].device_id, "emu-1");
    assert_eq!(opened[0].name, "Pixel 9");
    // Repeat snapshot does not re-open.
    assert!(tracker.apply(&state).is_empty());
}

#[test]
fn device_hub_parsers_cover_ax_tree_foreground_event_log_sse_and_fold() {
    use super::hub::*;

    // iOS AX tree skips screen-covering root and normalizes child frames.
    let ios_ax = parse_ios_ax_tree(&json!([
        {
            "type": "Application",
            "frame": { "x": 0, "y": 0, "width": 400, "height": 800 },
            "children": [
                { "type": "Button", "AXUniqueId": "login-btn", "AXLabel": "Sign In", "frame": { "x": 40, "y": 200, "width": 320, "height": 80 } }
            ]
        }
    ]));
    assert!(ios_ax.errors.is_empty());
    assert_eq!(ios_ax.elements.len(), 1);
    assert_eq!(ios_ax.elements[0].id, "login-btn");
    assert!((ios_ax.elements[0].x - 0.1).abs() < 0.001);
    assert!((ios_ax.elements[0].height - 0.1).abs() < 0.001);

    // Android AX tree filters screen-sized & unlabeled non-clickable nodes.
    let android_ax = parse_android_ax_tree(&json!({
        "nodes": [
            { "id": 0, "bounds": { "left": 0, "top": 0, "right": 1000, "bottom": 2000 } },
            { "id": 1, "text": "", "clickable": false, "bounds": { "left": 10, "top": 10, "right": 100, "bottom": 100 } },
            { "id": 2, "text": "Submit", "className": "android.widget.Button", "clickable": true, "bounds": { "left": 100, "top": 400, "right": 500, "bottom": 600 } }
        ]
    }));
    assert_eq!(android_ax.elements.len(), 1);
    assert_eq!(android_ax.elements[0].role, "Button");
    assert_eq!(android_ax.elements[0].label, "Submit");

    // Foreground SSE events.
    let fg = parse_foreground_event(&json!({ "bundleId": "com.acme.app", "pid": 42, "isReactNative": true })).unwrap().unwrap();
    assert_eq!(fg.id, "com.acme.app");
    assert_eq!(fg.pid, Some(42));
    assert_eq!(parse_foreground_event(&json!({ "bundleId": null })), Some(None));

    // Event log + rolling 100 limit.
    let (batch, reset) = parse_event_log_message(&json!({
        "events": [
            { "id": 1, "timestamp": "2026-11-19T12:34:56.000Z", "kind": "tap", "summary": "Tapped Login" }
        ]
    })).unwrap();
    assert!(reset);
    assert_eq!(batch[0].short_time(), "12:34:56");
    let many: Vec<EventLogEntry> = (0..120).map(|id| EventLogEntry { id, timestamp: String::new(), kind: "log".into(), summary: format!("e{id}") }).collect();
    let trimmed = append_event_log(&[], &many, true);
    assert_eq!(trimmed.len(), DEVICE_EVENT_LOG_LIMIT);
    assert_eq!(trimmed[0].id, 20);

    // SSE parser handles chunked and CRLF framing.
    let mut sse = SseParser::default();
    assert!(sse.push(b"data: {\"id\":1}\n").is_empty());
    assert_eq!(sse.push(b"\ndata: second\r\n\r\n"), vec!["{\"id\":1}", "second"]);

    // Android fold parser & angle.
    let fold = parse_android_fold(&json!({ "ok": true, "fold": { "supported": true, "posture": "closed", "hingeAngle": null } })).unwrap();
    assert!(fold.supported);
    assert_eq!(fold.angle(), 0.0);
    assert_eq!(FoldActionPosture::Opened.body(), json!({ "posture": "opened" }));
}
