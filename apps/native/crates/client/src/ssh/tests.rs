//! The process layer against a fake `ssh`: a shell script that records its arguments, answers
//! `-G`, runs the launch, pairing and stop scripts' roles, and for `-N` stands in for a tunnel.
//! The "remote" server is a `FakeServer` on the target's session port, so readiness checks reach
//! it without a real forward.

use std::path::PathBuf;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use super::*;
use crate::test_server::FakeServer;

const FAKE_SSH: &str = r#"#!/bin/bash
dir='@@DIR@@'
printf '%s\n' "$*" >> "$dir/calls"
started() {
  exec 3<>/dev/tcp/127.0.0.1/@@STATUS_PORT@@ || exit 1
  printf '%s %s %s\n' '@@NONCE@@' "$1" "$$" >&3
  read -r release <&3
  printf 'survived\n' >&3
  exit 1
}
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
const LAUNCH_HANGS: &str = "     started launch";

/// A tunnel that ignores SIGTERM, as a hung `ssh` can.
const TUNNEL_IGNORES_TERM: &str = "    trap '' TERM\n    started tunnel";

/// Another process holds the forwarded port.
const TUNNEL_PORT_TAKEN: &str = r#"    printf 'bind [127.0.0.1]:x: Address already in use\nchannel_setup_fwd_listener_tcpip: cannot listen to port\n' >&2
    exit 255"#;

/// A tunnel that stays up until it is signalled.
const TUNNEL_WAITS: &str = "    started tunnel";

/// Only the askpass helper's `hunter2` gets in.
const AUTH_PASSWORD: &str = r#"if [ "$("${SSH_ASKPASS:-false}" 2>/dev/null)" != 'hunter2' ]; then
  printf 'julius@devbox: Permission denied (publickey,password).\n' >&2
  exit 255
fi"#;

const AGENT_FAILURE: &str = "sign_and_send_pubkey: signing failed for ED25519 \"/home/julius/.ssh/id_ed25519\" from agent: communication with agent failed\njulius@devbox: Permission denied (publickey,password).";

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("t3-ssh-{name}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&path).expect("scratch dir");
        Self(path)
    }
}

impl std::ops::Deref for ScratchDir {
    type Target = std::path::Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    dir: ScratchDir,
    program: PathBuf,
    status: TcpListener,
    nonce: String,
}

struct FakeProcess {
    pid: i32,
    events: BufReader<TcpStream>,
}

impl FakeProcess {
    async fn stopped(mut self) {
        // A survivor reports itself when released; natural expiry cannot masquerade as a kill.
        self.events.get_mut().shutdown().await.expect("releases fake ssh");
        let mut remaining = String::new();
        self.events.read_to_string(&mut remaining).await.expect("reads fake ssh EOF");
        assert!(remaining.is_empty(), "fake ssh survived: {remaining}");
    }

    fn stopped_without_runtime(self) {
        use std::io::Read;
        let mut stream = self.events.into_inner().into_std().expect("status socket");
        stream.set_nonblocking(false).expect("blocking status socket");
        stream.shutdown(std::net::Shutdown::Write).expect("releases fake ssh");
        let mut remaining = String::new();
        // Keep Tokio's monitor unpolled: only shutdown's synchronous signal can kill this fake.
        stream.read_to_string(&mut remaining).expect("reads fake ssh EOF");
        assert!(remaining.is_empty(), "fake ssh survived synchronous shutdown: {remaining}");
    }
}

impl Fixture {
    fn new(name: &str, auth: &str, tunnel: &str) -> Fixture {
        Fixture::with_launch(name, auth, tunnel, "")
    }

    fn with_launch(name: &str, auth: &str, tunnel: &str, launch: &str) -> Fixture {
        use std::os::unix::fs::PermissionsExt;
        let dir = ScratchDir::new(name);
        let status = std::net::TcpListener::bind("127.0.0.1:0").expect("binds fake ssh status");
        let port = status.local_addr().expect("status address").port();
        status.set_nonblocking(true).expect("nonblocking listener");
        let status = TcpListener::from_std(status).expect("status listener");
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let program = dir.join("ssh");
        let script = FAKE_SSH
            .replace("@@DIR@@", &dir.display().to_string())
            .replace("@@NONCE@@", &nonce)
            .replace("@@AUTH@@", auth)
            .replace("@@TUNNEL@@", tunnel)
            .replace("@@LAUNCH@@", launch)
            .replace("@@STATUS_PORT@@", &port.to_string());
        std::fs::write(&program, script).expect("writes the fake ssh");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Fixture { dir, program, status, nonce }
    }

    fn lines(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(file)).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    async fn started<F: Future>(&self, role: &str, finished: F) -> Result<FakeProcess, F::Output> {
        tokio::select! {
            biased;
            result = finished => Err(result),
            process = self.startup_event(role) => Ok(process),
        }
    }

    async fn tunnel(&self, ssh: &SshEnvironments) -> FakeProcess {
        let mut exit = ssh.state().tunnels.values().next().expect("a connected tunnel").exit.clone();
        self.started("tunnel", async move { exit.wait_for(Option::is_some).await.map(|code| *code) }).await.expect("tunnel exited before announcing startup")
    }

    async fn startup_event(&self, role: &str) -> FakeProcess {
        let mut readers = tokio::task::JoinSet::new();
        let prefix = format!("{} {role} ", self.nonce);
        loop {
            tokio::select! {
                connection = self.status.accept() => {
                    let (stream, _) = connection.expect("accepts fake ssh");
                    readers.spawn(async move {
                        let mut events = BufReader::new(stream);
                        let mut line = String::new();
                        if events.read_line(&mut line).await.ok()? == 0 {
                            return None;
                        }
                        Some((events, line))
                    });
                }
                finished = readers.join_next(), if !readers.is_empty() => {
                    let Some(Ok(Some((events, line)))) = finished else { continue };
                    let Some(pid) = line.strip_prefix(&prefix).and_then(|line| line.strip_suffix('\n')).and_then(|pid| pid.parse().ok()) else { continue };
                    assert!(alive(pid), "fake ssh is alive before stopping it");
                    return FakeProcess { pid, events };
                }
            }
        }
    }

    fn manager(&self, prompts: Option<mpsc::UnboundedSender<PasswordRequest>>) -> SshEnvironments {
        SshEnvironments::with_program(self.program.clone(), prompts, Some("0.1.7".to_owned()))
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

async fn devbox_server() -> FakeServer {
    let server = FakeServer::start().await;
    server.set_descriptor(Some(json!({ "environmentId": "env-devbox", "label": "devbox", "serverVersion": "0.1.7", "orchestrationProtocolVersion": 2 })));
    server
}

#[tokio::test]
async fn provisioning_launches_the_server_forwards_a_port_and_pairs_over_ssh() {
    let server = devbox_server().await;
    let fixture = Fixture::new("provision", "", TUNNEL_WAITS);
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
    let tunnel = fixture.tunnel(&ssh).await;
    let pid = tunnel.pid;
    assert!(alive(pid));
    ssh.disconnect(&pairing.target).await.expect("disconnects");
    tunnel.stopped().await;
    assert!(!alive(pid), "the tunnel is stopped and reaped");
    assert_eq!(fixture.lines("stops").len(), 1);

    // Quitting stops tunnels and leaves the remote server for the next launch.
    ssh.ensure(&pairing.target, false).await.expect("connects again");
    let tunnel = fixture.tunnel(&ssh).await;
    let pid = tunnel.pid;
    ssh.shutdown().await;
    tunnel.stopped().await;
    assert!(!alive(pid));
    assert_eq!(fixture.lines("stops").len(), 1);
}

#[tokio::test]
async fn a_password_host_asks_in_the_app_and_retries_through_askpass() {
    let server = devbox_server().await;
    let fixture = Fixture::new("password", AUTH_PASSWORD, TUNNEL_WAITS);
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
    let fixture = Fixture::new("agent", &format!("printf '%s\\n' '{}' >&2\nexit 255", AGENT_FAILURE.replace('\n', "' '")), TUNNEL_WAITS);
    let (prompts, mut requests) = mpsc::unbounded_channel();
    let ssh = fixture.manager(Some(prompts));
    let Err(PrepareError::Transient(message)) = ssh.provision(&devbox(), None).await else { panic!("fails without asking") };
    assert!(message.starts_with("Could not prepare the SSH environment: sign_and_send_pubkey: signing failed"), "{message}");
    assert!(requests.try_recv().is_err(), "no prompt for an agent failure");

    // A cancelled prompt waits for the user instead of retrying.
    let fixture = Fixture::new("cancel", AUTH_PASSWORD, TUNNEL_WAITS);
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
async fn startup_returns_the_operation_error_when_the_fake_exits_before_announcing() {
    for (name, role, launch, tunnel, diagnostic) in [
        ("launch-exit", "launch", "printf 'fixture exited before startup\\n' >&2; exit 73", TUNNEL_WAITS, "fixture exited before startup"),
        ("tunnel-exit", "tunnel", "", "printf 'fixture exited before startup\\n' >&2; exit 73", "fixture exited before startup"),
        ("startup-eof", "launch", "exec 3<>/dev/tcp/127.0.0.1/@@STATUS_PORT@@; exit 73", TUNNEL_WAITS, "73"),
        ("startup-invalid", "launch", "exec 3<>/dev/tcp/127.0.0.1/@@STATUS_PORT@@; printf 'not a startup event\\n' >&3; exit 73", TUNNEL_WAITS, "73"),
        ("spawn-failure", "launch", "", TUNNEL_WAITS, "Failed to spawn SSH command"),
    ] {
        let fixture = Fixture::with_launch(name, "", tunnel, launch);
        if name == "spawn-failure" {
            std::fs::write(&fixture.program, "#!/nonexistent/ssh-test-interpreter\n").expect("writes missing interpreter");
        }
        let ssh = fixture.manager(None);
        let result = ssh.ensure(&devbox(), false).await;
        assert!(result.is_err(), "fake must exit before startup");
        let mut startup = Box::pin(fixture.started(role, std::future::ready(result)));
        let std::task::Poll::Ready(Err(Err(SshError(message)))) = futures::poll!(&mut startup) else {
            panic!("startup must return the completed operation's error instead of waiting forever");
        };
        assert!(message.contains(diagnostic), "{message}");
        drop(startup);

        let Err(Err(SshError(message))) = fixture.started(role, ssh.ensure(&devbox(), false)).await else {
            panic!("startup must also drive and report an in-flight operation's error");
        };
        assert!(message.contains(diagnostic), "{message}");
    }
}

#[tokio::test]
async fn quitting_stops_ssh_still_starting_the_server() {
    let fixture = Fixture::with_launch("quit", "", TUNNEL_WAITS, LAUNCH_HANGS);
    let address = fixture.status.local_addr().expect("status address");
    let mut unrelated = TcpStream::connect(address).await.expect("unrelated peer");
    unrelated.write_all(b"HEAD / HTTP/1.1\r\n").await.expect("unrelated request");
    let _silent = TcpStream::connect(address).await.expect("silent peer");
    let ssh = fixture.manager(None);
    let mut starting = tokio::spawn({
        let ssh = ssh.clone();
        async move { ssh.ensure(&devbox(), false).await }
    });
    let launch = fixture.started("launch", &mut starting).await.expect("launch operation ended before announcing startup");
    ssh.shutdown().await;
    launch.stopped().await;
    assert_eq!(starting.await.expect("joins").err(), Some(quitting()));

    // A tunnel still waiting to answer stops too, and nothing new starts after the quit.
    let fixture = Fixture::new("quit-tunnel", "", TUNNEL_WAITS);
    let ssh = fixture.manager(None);
    let mut starting = tokio::spawn({
        let ssh = ssh.clone();
        async move { ssh.ensure(&devbox(), false).await }
    });
    let tunnel = fixture.started("tunnel", &mut starting).await.expect("tunnel operation ended before announcing startup");
    ssh.shutdown().await;
    tunnel.stopped().await;
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
    // The config gains or changes a HostName while connected.
    for (name, original) in [("moved", "devbox.example.com"), ("moved-plain", "devbox")] {
        let server = devbox_server().await;
        let fixture = Fixture::new(name, "", TUNNEL_WAITS);
        std::fs::write(fixture.dir.join("hostname"), original).expect("writes");
        let ssh = fixture.manager(None);
        forward_to(&ssh, &devbox(), &server);
        let bootstrap = ssh.ensure(&devbox(), false).await.expect("connects");
        let tunnel = fixture.tunnel(&ssh).await;
        let pid = tunnel.pid;
        std::fs::write(fixture.dir.join("hostname"), "devbox-new.example.com").expect("writes");
        ssh.disconnect(&bootstrap.target).await.expect("disconnects");
        tunnel.stopped().await;
        assert!(!alive(pid), "the running tunnel is found and stopped");
        let stops = fixture.lines("stops");
        assert!(stops.len() == 1 && stops[0].contains(&format!("-o HostName={original} ")), "the stop reaches the host the server runs on: {stops:?}");
        let resolves: Vec<String> = fixture.lines("calls").into_iter().filter(|call| call.contains(" -G ")).collect();
        assert!(resolves.iter().all(|call| !call.contains("HostName=")), "resolving asks ssh, unpinned: {resolves:?}");
    }
}

#[tokio::test]
async fn quitting_kills_a_tunnel_that_ignores_sigterm_before_returning() {
    let server = devbox_server().await;
    let fixture = Fixture::new("quit-term", "", TUNNEL_IGNORES_TERM);
    let ssh = fixture.manager(None);
    forward_to(&ssh, &devbox(), &server);
    ssh.ensure(&devbox(), false).await.expect("connects");
    let tunnel = fixture.tunnel(&ssh).await;
    drop(ssh.shutdown());
    tunnel.stopped_without_runtime();
}

#[test]
fn the_stop_script_signals_only_the_server_it_started() {
    use std::io::Write;
    let home = ScratchDir::new("stop");
    let state = home.join(".t3/ssh-launch/native-k");
    std::fs::create_dir_all(&state).expect("state dir");
    let mut server = std::process::Command::new("/bin/sh").args(["-c", "read -r release"]).stdin(Stdio::piped()).spawn().expect("server runs");
    let pid = i32::try_from(server.id()).expect("server pid");
    let _input = server.stdin.take().expect("server stdin");
    // Reap concurrently, as the remote server's parent would while the stop script runs.
    let reaped = std::thread::spawn(move || server.wait().expect("server exits"));
    let started = std::process::Command::new("ps").args(["-o", "lstart=", "-p", &pid.to_string()]).output().expect("ps runs");
    let started = String::from_utf8_lossy(&started.stdout).trim_end_matches('\n').to_owned();
    // A `ps` that cannot tell, as on a host without one.
    let no_ps = home.join("no-ps");
    std::fs::create_dir_all(&no_ps).expect("dir");
    std::fs::write(no_ps.join("ps"), "#!/bin/sh\nexit 1\n").expect("writes");
    std::fs::set_permissions(no_ps.join("ps"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
    let stop_with = |recorded_start: &str, path: Option<&std::path::Path>| {
        std::fs::write(state.join("pid"), format!("{pid}\n")).expect("pid");
        std::fs::write(state.join("managed"), "managed\n").expect("managed");
        std::fs::write(state.join("started"), format!("{recorded_start}\n")).expect("started");
        let mut command = std::process::Command::new("/bin/sh");
        if let Some(path) = path {
            command.env("PATH", format!("{}:/usr/bin:/bin", path.display()));
        }
        let mut sh = command.arg("-s").env("HOME", &home.0).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("sh runs");
        sh.stdin.take().expect("stdin").write_all(scripts::stop_script("native-k").as_bytes()).expect("writes");
        sh.wait_with_output().expect("sh exits")
    };
    let stop = |recorded_start: &str| stop_with(recorded_start, None);
    // The recorded PID now belongs to another process: it is left alone, and the files go.
    let output = stop("Thu Jan  1 00:00:00 1970");
    assert!(output.status.success() && running(pid), "a reused PID is not signalled");
    assert!(!state.join("pid").exists() && !state.join("started").exists());
    // Without a start time to compare, nothing is signalled either.
    let output = stop_with("", Some(&no_ps));
    assert!(output.status.success() && running(pid), "no start time, no signal");
    // The server it started, by PID and start time, is stopped.
    let output = stop(&started);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(!running(pid), "its own server is stopped");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(reaped.join().expect("reaps server").signal(), Some(libc::SIGTERM));
}
