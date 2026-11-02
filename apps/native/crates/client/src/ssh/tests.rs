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
  *" -G "*) printf 'hostname devbox.example.com\nuser julius\nport 2222\n'; exit 0 ;;
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
  *) printf '%s\n' "$*" >> "$dir/launches"; printf 'loaded nvm default\n{"remotePort":3774}\n' ;;
esac
"#;

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
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("t3-ssh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let program = dir.join("ssh");
        let script = FAKE_SSH.replace("@@DIR@@", &dir.display().to_string()).replace("@@AUTH@@", auth).replace("@@TUNNEL@@", tunnel);
        std::fs::write(&program, script).expect("writes the fake ssh");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Fixture { dir, program }
    }

    fn lines(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(file)).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    fn tunnel_pid(&self) -> i32 {
        std::fs::read_to_string(self.dir.join("tunnel.pid")).ok().and_then(|pid| pid.trim().parse().ok()).expect("the tunnel recorded its pid")
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
    ssh.prepare(&pairing.target, Some("env-devbox")).await.expect("reuses the tunnel");
    assert_eq!(fixture.lines("calls").iter().filter(|call| call.contains(" -N ")).count(), 1);
    assert_eq!(
        ssh.prepare(&pairing.target, Some("env-other")).await,
        Err(PrepareError::Blocked("That host reaches devbox, a different machine. Add it as its own environment instead.".to_owned()))
    );

    // Disconnect stops the tunnel the app spawned and the remote server.
    let pid = fixture.tunnel_pid();
    assert!(alive(pid));
    ssh.disconnect(&pairing.target).await.expect("disconnects");
    assert!(!alive(pid), "the tunnel is stopped and reaped");
    assert_eq!(fixture.lines("stops").len(), 1);

    // Quitting stops tunnels and leaves the remote server for the next launch.
    ssh.ensure(&pairing.target, false).await.expect("connects again");
    let pid = fixture.tunnel_pid();
    for monitor in ssh.shutdown() {
        monitor.await.expect("the monitor finishes");
    }
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
