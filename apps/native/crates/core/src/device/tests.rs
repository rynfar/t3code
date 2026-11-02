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
    assert_eq!(eas.platform_setup_status(Platform::Android).message, "eas host starts Android emulators in the cloud with EAS Simulator.");
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
