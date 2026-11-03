//! The process layer against a fake `ssh`: a shell script that records its arguments, answers
//! `-G`, runs the launch, pairing and stop scripts' roles, and for `-N` stands in for a tunnel.
//! The "remote" server is a `FakeServer` on the target's session port, so readiness checks reach
//! it without a real forward.

use std::path::PathBuf;

use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::test_server::FakeServer;

const FAKE_SSH: &str = r#"#!/bin/sh
dir='@@DIR@@'
printf '%s\n' "$*" >> "$dir/calls"
case " $* " in
  *" -G "*) printf 'hostname %s\nuser julius\nport 2222\n' "$(cat "$dir/hostname" 2>/dev/null || echo devbox.example.com)"; exit 0 ;;
esac
@@AUTH@@
case " $* " in
  *" -N "*)
@@TUNNEL@@
    ;;
esac
input="$(cat)"
case "$input" in
  *'auth pairing create'*) printf 'loaded nvm default\n{"credential":"pairing-secret"}\n' ;;
  *'"stopped":true'*) printf '%s\n' "$*" >> "$dir/stops"; printf '{"stopped":true}\n' ;;
  *) printf '%s\n' "$*" >> "$dir/launches"
@@LAUNCH@@
     printf 'loaded nvm default\n{"remotePort":3774}\n' ;;
esac
"#;

/// A launch still downloading the release.
const LAUNCH_HANGS: &str = r#"     printf '%s\n' "$$" > "$dir/launch.pid"
     exec sleep 600"#;

/// Another process holds the forwarded port.
const TUNNEL_PORT_TAKEN: &str = r#"    printf 'bind [127.0.0.1]:x: Address already in use\nchannel_setup_fwd_listener_tcpip: cannot listen to port\n' >&2
    exit 255"#;

/// A tunnel that stays up until it is signalled.
const TUNNEL_SLEEPS: &str = r#"    printf '%s\n' "$$" > "$dir/tunnel.pid"
    exec sleep 600"#;

/// Only the askpass helper's `hunter2` gets in.
const AUTH_PASSWORD: &str = r#"if [ "$("${SSH_ASKPASS:-false}" 2>/dev/null)" != 'hunter2' ]; then
  printf 'julius@devbox: Permission denied (publickey,password).\n' >&2
  exit 255
fi"#;

const AGENT_FAILURE: &str = "sign_and_send_pubkey: signing failed for ED25519 \"/home/julius/.ssh/id_ed25519\" from agent: communication with agent failed\njulius@devbox: Permission denied (publickey,password).";

struct Fixture {
    dir: PathBuf,
    program: PathBuf,
}

impl Fixture {
    fn new(name: &str, auth: &str, tunnel: &str) -> Fixture {
        Fixture::with_launch(name, auth, tunnel, "")
    }

    fn with_launch(name: &str, auth: &str, tunnel: &str, launch: &str) -> Fixture {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("t3-ssh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let program = dir.join("ssh");
        let script = FAKE_SSH.replace("@@DIR@@", &dir.display().to_string()).replace("@@AUTH@@", auth).replace("@@TUNNEL@@", tunnel).replace("@@LAUNCH@@", launch);
        std::fs::write(&program, script).expect("writes the fake ssh");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Fixture { dir, program }
    }

    fn lines(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(file)).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    /// The PID a fake recorded once it started. Bounded, so a fake that never got there fails.
    async fn recorded_pid(&self, file: &str) -> i32 {
        for _ in 0..200 {
            if let Some(pid) = std::fs::read_to_string(self.dir.join(file)).ok().and_then(|pid| pid.trim().parse().ok()) {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{file} was never recorded");
    }

    fn manager(&self, prompts: Option<mpsc::UnboundedSender<PasswordRequest>>) -> SshEnvironments {
        SshEnvironments::with_program(self.program.clone(), prompts, Some("0.1.7".to_owned()))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn devbox() -> SshTarget {
    SshTarget { alias: "devbox".into(), hostname: "devbox".into(), username: None, port: None }
}

/// Points the target's session port at the fake server, which plays the remote through the tunnel.
fn forward_to(ssh: &SshEnvironments, target: &SshTarget, server: &FakeServer) -> u16 {
    ssh.state().ports.insert(core::target_connection_key(target), server.port());
    server.port()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; it only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Running, as opposed to gone or a zombie waiting to be reaped.
fn running(pid: i32) -> bool {
    let stat = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().map(|output| String::from_utf8_lossy(&output.stdout).into_owned());
    alive(pid) && !stat.unwrap_or_default().contains('Z')
}

/// Whether the process stops within a few seconds; a killed child is reaped asynchronously.
async fn stops(pid: i32) -> bool {
    for _ in 0..100 {
        if !running(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn devbox_server() -> FakeServer {
    let server = FakeServer::start().await;
    server.set_descriptor(Some(json!({ "environmentId": "env-devbox", "label": "devbox", "serverVersion": "0.1.7", "orchestrationProtocolVersion": 2 })));
    server
}

#[tokio::test]
async fn provisioning_launches_the_server_forwards_a_port_and_pairs_over_ssh() {
    let server = devbox_server().await;
    let fixture = Fixture::new("provision", "", TUNNEL_SLEEPS);
    let ssh = fixture.manager(None);
    let port = forward_to(&ssh, &devbox(), &server);

    let pairing = ssh.provision(&devbox(), None).await.expect("pairs");
    let resolved = SshTarget { alias: "devbox".into(), hostname: "devbox.example.com".into(), username: Some("julius".into()), port: Some(2222) };
    assert_eq!(pairing.target, resolved);
    assert_eq!(pairing.paired.credentials.http_base, format!("http://127.0.0.1:{port}"));
    assert_eq!(pairing.paired.credentials.access_token, "ssh-access");
    assert_eq!(pairing.paired.descriptor.map(|descriptor| descriptor.environment_id), Some("env-devbox".to_owned()));
    let launches = fixture.lines("launches");
    assert_eq!(launches.len(), 1);
    assert!(launches[0].ends_with(&format!("-p 2222 -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -- julius@devbox sh -l -s -- {}", core::native_state_key(&resolved))), "{}", launches[0]);
    let tunnels: Vec<String> = fixture.lines("calls").into_iter().filter(|call| call.contains(" -N ")).collect();
    assert_eq!(tunnels.len(), 1);
    assert!(tunnels[0].ends_with(&format!("-L {port}:127.0.0.1:3774 -- julius@devbox")), "{}", tunnels[0]);

    // The supervisor's step reuses the tunnel through the resolved target, and checks identity.
    let address = format!("http://127.0.0.1:{port}");
    ssh.prepare(&pairing.target, Some("env-devbox"), &address).await.expect("reuses the tunnel");
    assert_eq!(fixture.lines("calls").iter().filter(|call| call.contains(" -N ")).count(), 1);
    assert_eq!(
        ssh.prepare(&pairing.target, Some("env-other"), &address).await,
        Err(PrepareError::Blocked("That host reaches devbox, a different machine. Add it as its own environment instead.".to_owned()))
    );
    // A client still pointed at an older port learns where the tunnel is now.
    assert_eq!(ssh.prepare(&pairing.target, Some("env-devbox"), "http://127.0.0.1:1").await, Err(PrepareError::Moved(address.clone())));

    // Disconnect stops the tunnel the app spawned and the remote server.
    let pid = fixture.recorded_pid("tunnel.pid").await;
    assert!(alive(pid));
    ssh.disconnect(&pairing.target).await.expect("disconnects");
    assert!(!alive(pid), "the tunnel is stopped and reaped");
    assert_eq!(fixture.lines("stops").len(), 1);

    // Quitting stops tunnels and leaves the remote server for the next launch.
    let _ = std::fs::remove_file(fixture.dir.join("tunnel.pid"));
    ssh.ensure(&pairing.target, false).await.expect("connects again");
    let pid = fixture.recorded_pid("tunnel.pid").await;
    ssh.shutdown().await;
    assert!(!alive(pid));
    assert_eq!(fixture.lines("stops").len(), 1);
}

#[tokio::test]
async fn a_password_host_asks_in_the_app_and_retries_through_askpass() {
    let server = devbox_server().await;
    let fixture = Fixture::new("password", AUTH_PASSWORD, TUNNEL_SLEEPS);
    let (prompts, mut requests) = mpsc::unbounded_channel();
    let ssh = fixture.manager(Some(prompts));
    forward_to(&ssh, &devbox(), &server);
    let answers = tokio::spawn(async move {
        let mut asked = Vec::new();
        while let Some(request) = requests.recv().await {
            asked.push((request.attempt, request.prompt.clone(), request.destination.clone(), request.username.clone()));
            // The first answer is wrong, so the second prompt follows.
            let answer = if asked.len() == 1 { "wrong" } else { "hunter2" };
            assert!(request.is_pending());
            request.respond(Some(answer.to_owned()));
        }
        asked
    });

    let pairing = ssh.provision(&devbox(), None).await.expect("pairs after the password");
    assert_eq!(pairing.paired.credentials.access_token, "ssh-access");
    // The tunnel and the pairing reused the answer without asking again.
    drop(ssh);
    let asked = answers.await.expect("answers");
    let prompt = "Enter the SSH password or verification code for julius@devbox.".to_owned();
    assert_eq!(
        asked,
        [(1, prompt.clone(), "devbox".to_owned(), Some("julius".to_owned())), (2, prompt, "devbox".to_owned(), Some("julius".to_owned()))]
    );
}

#[tokio::test]
async fn auth_failures_a_password_cannot_fix_or_a_cancelled_prompt_stop_the_attempt() {
    // The local agent could not sign: no prompt, and the failure is ssh's own words.
    let fixture = Fixture::new("agent", &format!("printf '%s\\n' '{}' >&2\nexit 255", AGENT_FAILURE.replace('\n', "' '")), TUNNEL_SLEEPS);
    let (prompts, mut requests) = mpsc::unbounded_channel();
    let ssh = fixture.manager(Some(prompts));
    let Err(PrepareError::Transient(message)) = ssh.provision(&devbox(), None).await else { panic!("fails without asking") };
    assert!(message.starts_with("Could not prepare the SSH environment: sign_and_send_pubkey: signing failed"), "{message}");
    assert!(requests.try_recv().is_err(), "no prompt for an agent failure");

    // A cancelled prompt waits for the user instead of retrying.
    let fixture = Fixture::new("cancel", AUTH_PASSWORD, TUNNEL_SLEEPS);
    let (prompts, mut requests) = mpsc::unbounded_channel::<PasswordRequest>();
    let ssh = fixture.manager(Some(prompts));
    tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            request.respond(None);
        }
    });
    assert_eq!(ssh.provision(&devbox(), None).await.err(), Some(PrepareError::Blocked("SSH authentication cancelled for julius@devbox.".to_owned())));
}

#[tokio::test]
async fn a_tunnel_that_exits_before_answering_reports_a_bounded_tail_of_its_stderr() {
    let tunnel = "    printf 'old diagnostics\\n' >&2\n    head -c 40000 /dev/zero | tr '\\0' x >&2\n    printf '\\nConnection refused: café' >&2\n    exit 1";
    let fixture = Fixture::new("exit", "", tunnel);
    let ssh = fixture.manager(None);
    let Err(SshError(message)) = ssh.ensure(&devbox(), false).await else { panic!("the tunnel fails") };
    assert_eq!(message.chars().count(), core::TUNNEL_STDERR_TAIL_CHARS);
    assert!(message.ends_with("Connection refused: café"), "{}", &message[message.len() - 40..]);
    assert!(ssh.state().tunnels.is_empty());
}

#[tokio::test]
async fn the_alias_stays_as_typed_when_ssh_cannot_resolve_it() {
    let ssh = SshEnvironments::with_program(PathBuf::from("/nonexistent/ssh"), None, None);
    assert_eq!(ssh.resolve_target(" devbox ").await, Ok(devbox()));
    assert_eq!(ssh.resolve_target(" ").await, Err(SshError("SSH host alias is required.".to_owned())));
}

#[test]
fn the_remote_scripts_parse_as_posix_shell() {
    use std::io::Write;
    let scripts = [scripts::launch_script("0.1.7"), scripts::pairing_script("native-k", "0.1.7"), Ok(scripts::stop_script("native-k")), scripts::runner_script("0.1.7")];
    for script in scripts {
        let mut child = std::process::Command::new("/bin/sh").arg("-n").stdin(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("sh runs");
        child.stdin.take().expect("stdin").write_all(script.expect("builds").as_bytes()).expect("writes");
        let output = child.wait_with_output().expect("sh exits");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
}

#[test]
fn the_askpass_helper_prints_the_cached_answer() {
    let helper = askpass_helper().expect("writes the helper");
    assert_eq!(askpass_helper().expect("reuses it"), helper);
    let run = |secret: Option<&str>| {
        let mut command = std::process::Command::new(&helper);
        command.env_remove("T3_SSH_AUTH_SECRET");
        if let Some(secret) = secret {
            command.env("T3_SSH_AUTH_SECRET", secret);
        }
        command.output().expect("runs")
    };
    assert_eq!(String::from_utf8_lossy(&run(Some("pw")).stdout), "pw\n");
    assert!(!run(None).status.success(), "a missing secret fails loudly");
}

#[tokio::test]
async fn quitting_stops_ssh_still_starting_the_server() {
    let fixture = Fixture::with_launch("quit", "", TUNNEL_SLEEPS, LAUNCH_HANGS);
    let ssh = fixture.manager(None);
    let starting = tokio::spawn({
        let ssh = ssh.clone();
        async move { ssh.ensure(&devbox(), false).await }
    });
    let launch = fixture.recorded_pid("launch.pid").await;
    ssh.shutdown().await;
    assert!(stops(launch).await, "the launch that was still installing is stopped");
    assert_eq!(starting.await.expect("joins").err(), Some(quitting()));

    // A tunnel still waiting to answer stops too, and nothing new starts after the quit.
    let fixture = Fixture::new("quit-tunnel", "", TUNNEL_SLEEPS);
    let ssh = fixture.manager(None);
    let starting = tokio::spawn({
        let ssh = ssh.clone();
        async move { ssh.ensure(&devbox(), false).await }
    });
    let tunnel = fixture.recorded_pid("tunnel.pid").await;
    ssh.shutdown().await;
    assert!(stops(tunnel).await, "the tunnel that never answered is stopped");
    assert!(starting.await.expect("joins").is_err());
    assert_eq!(ssh.ensure(&devbox(), false).await.err(), Some(quitting()));
}

#[tokio::test]
async fn a_forwarded_port_another_process_took_is_replaced() {
    let fixture = Fixture::new("port", "", TUNNEL_PORT_TAKEN);
    let ssh = fixture.manager(None);
    let taken = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("binds");
    let busy = taken.local_addr().expect("address").port();
    ssh.state().ports.insert(core::target_connection_key(&devbox()), busy);
    let _ = ssh.ensure(&devbox(), false).await;
    let tunnels: Vec<String> = fixture.lines("calls").into_iter().filter(|call| call.contains(" -N ")).collect();
    assert_eq!(tunnels.len(), 2, "one retry on another port: {tunnels:?}");
    assert!(tunnels[0].contains(&format!("-L {busy}:")) && !tunnels[1].contains(&format!("-L {busy}:")), "{tunnels:?}");
    let moved = ssh.session_port(&devbox()).expect("a port");
    assert!(moved != busy && tunnels[1].contains(&format!("-L {moved}:")), "the session moves to the new port");
}

#[tokio::test]
async fn disconnect_stops_the_tunnel_it_started_after_the_config_moves_the_alias() {
    let server = devbox_server().await;
    let fixture = Fixture::new("moved", "", TUNNEL_SLEEPS);
    let ssh = fixture.manager(None);
    forward_to(&ssh, &devbox(), &server);
    let bootstrap = ssh.ensure(&devbox(), false).await.expect("connects");
    let pid = fixture.recorded_pid("tunnel.pid").await;
    // The alias now resolves to another host.
    std::fs::write(fixture.dir.join("hostname"), "devbox-new.example.com").expect("writes");
    ssh.disconnect(&bootstrap.target).await.expect("disconnects");
    assert!(!alive(pid), "the running tunnel is found and stopped");
    let stops = fixture.lines("stops");
    assert!(stops.len() == 1 && stops[0].contains("-o HostName=devbox.example.com"), "the stop reaches the host the server runs on: {stops:?}");
}

#[test]
fn the_stop_script_signals_only_the_server_it_started() {
    use std::io::Write;
    let home = std::env::temp_dir().join(format!("t3-ssh-stop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let state = home.join(".t3/ssh-launch/native-k");
    std::fs::create_dir_all(&state).expect("state dir");
    // Detached, as the nohup'd remote server is, so it is reaped once it stops.
    let spawned = std::process::Command::new("/bin/sh").args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"]).output().expect("sh runs");
    let pid: i32 = String::from_utf8_lossy(&spawned.stdout).trim().parse().expect("a pid");
    let started = std::process::Command::new("ps").args(["-o", "lstart=", "-p", &pid.to_string()]).output().expect("ps runs");
    let started = String::from_utf8_lossy(&started.stdout).trim_end_matches('\n').to_owned();
    let stop = |recorded_start: &str| {
        std::fs::write(state.join("pid"), format!("{pid}\n")).expect("pid");
        std::fs::write(state.join("managed"), "managed\n").expect("managed");
        std::fs::write(state.join("started"), format!("{recorded_start}\n")).expect("started");
        let mut sh = std::process::Command::new("/bin/sh").arg("-s").env("HOME", &home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("sh runs");
        sh.stdin.take().expect("stdin").write_all(scripts::stop_script("native-k").as_bytes()).expect("writes");
        sh.wait_with_output().expect("sh exits")
    };
    // The recorded PID now belongs to another process: it is left alone, and the files go.
    let output = stop("Thu Jan  1 00:00:00 1970");
    assert!(output.status.success() && running(pid), "a reused PID is not signalled");
    assert!(!state.join("pid").exists() && !state.join("started").exists());
    // The server it started, by PID and start time, is stopped.
    let output = stop(&started);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(!running(pid), "its own server is stopped");
    let still_ours = std::process::Command::new("ps").args(["-o", "lstart=", "-p", &pid.to_string()]).output().is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim_end_matches('\n') == started);
    if still_ours {
        // SAFETY: a plain syscall on the sleep this test started, identified by its start time.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    let _ = std::fs::remove_dir_all(&home);
}
