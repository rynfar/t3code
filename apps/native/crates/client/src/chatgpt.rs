//! Network sequence from `web/src/components/settings/CodexSetupSection.tsx`.
//! The caller owns cancellation by aborting the task; transferred profiles stay in memory.
use crate::{
    auth::Credentials,
    session::{Session, StreamHandle},
    supervisor::ClientHandle,
};
use serde_json::Value;
use t3_core::settings::providers::{auth, chatgpt as model};
use tokio::sync::mpsc;

pub enum Update {
    Auth(auth::AuthState),
    InstallStarted(auth::InstallState),
    Install(auth::InstallState),
    Page(String),
    Handoff,
    Finishing,
    TransferFailed,
}

impl Update {
    pub fn apply_install(&self, state: &mut model::SetupState) {
        match self {
            Self::InstallStarted(install) => state.started_install(install.clone()),
            Self::Install(install) => state.observe_install(install.clone()),
            _ => {}
        }
    }
}

pub struct SetupRequest {
    pub instance: String,
    pub method: String,
    pub environment: String,
    pub return_url: String,
    pub local: bool,
    pub helper: Option<Credentials>,
    pub install: auth::InstallState,
}

async fn item(stream: &mut StreamHandle) -> Result<Value, String> {
    stream.receiver.recv().await.ok_or_else(|| "Codex setup was interrupted. Try again.".to_owned())?.map_err(|_| "Codex setup failed. Try again.".to_owned())
}

pub async fn setup(client: ClientHandle, request: SetupRequest, updates: mpsc::UnboundedSender<Update>, mut retry: mpsc::UnboundedReceiver<()>) -> Result<(), String> {
    let SetupRequest { instance, method, environment, return_url, local, helper, install } = request;
    if model::needs_install(&install) {
        let mut stream = client.stream(auth::INSTALL_SUBSCRIBE, auth::subscribe_payload(&instance)).await.map_err(|_| "Could not read runtime status.")?;
        let started =
            client.request(auth::INSTALL_START, auth::subscribe_payload(&instance)).await.ok().and_then(auth::decode_install_state).ok_or("Codex setup failed. Try again.")?;
        let _ = updates.send(Update::InstallStarted(started.clone()));
        let mut current = started.clone();
        loop {
            match model::settle_install(&started, Some(&current)) {
                Some(true) => break,
                Some(false) => {
                    return Err(current.message.unwrap_or_else(|| "Codex installation did not finish.".into()));
                }
                None => {}
            }
            current = auth::decode_install_state(item(&mut stream).await?).ok_or("Could not read runtime status.")?;
            let _ = updates.send(Update::Install(current.clone()));
        }
    }
    let helper_session = match helper {
        Some(helper) => match crate::auth::issue_rpc_ticket(&helper).await {
            Ok(ticket) => Session::connect(&crate::auth::socket_url(&helper, &ticket)).await.ok(),
            Err(_) => None,
        },
        None => None,
    };
    if let Some(session) = helper_session {
        let _ = updates.send(Update::Handoff);
        let profile = client.request(model::RECONNECT, model::reconnect_payload(&instance, &method)).await.map_err(|_| "Could not prepare ChatGPT sign-in.")?;
        let profile: Option<model::ReconnectProfile> = serde_json::from_value(profile).map_err(|_| "Could not read ChatGPT profile.")?;
        // One fresh attempt per task; a cancelled task cannot import its result later.
        let attempt = format!("native-{}-{}", std::process::id(), NEXT_ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let mut stream = session
            .stream(model::HANDOFF, model::handoff_payload(&instance, &environment, &attempt, &return_url, profile.as_ref()))
            .map_err(|_| "Could not start ChatGPT handoff.")?;
        let mut opened = None;
        loop {
            match serde_json::from_value::<model::HandoffState>(item(&mut stream).await?).map_err(|_| "Could not read ChatGPT handoff.")? {
                model::HandoffState::Auth { state } => {
                    if state.phase == "failed" || state.phase == "cancelled" {
                        return Err(state.message.unwrap_or_else(|| "ChatGPT sign-in could not finish. Try again.".into()));
                    }
                    if let Some(Some(page)) = model::settle_page(Some(&state), None)
                        && opened.as_ref() != Some(&page)
                    {
                        opened = Some(page.clone());
                        let _ = updates.send(Update::Page(page));
                    }
                    let _ = updates.send(Update::Auth(state));
                }
                model::HandoffState::Finished { profile } => {
                    loop {
                        if client.request(model::IMPORT, model::import_payload(&instance, &profile)).await.is_ok() {
                            break;
                        }
                        let _ = updates.send(Update::TransferFailed);
                        retry.recv().await.ok_or("ChatGPT connection cancelled.")?;
                    }
                    break;
                }
                model::HandoffState::Unknown => {}
            }
        }
    } else {
        let mut stream = client.stream(auth::AUTH_SUBSCRIBE, auth::subscribe_payload(&instance)).await.map_err(|_| "Could not read sign-in status.")?;
        let started = client
            .request(auth::AUTH_START, model::start_payload(&instance, &method, &return_url, local))
            .await
            .ok()
            .and_then(auth::decode_auth_state)
            .ok_or("Codex setup failed. Try again.")?;
        let flow = started.flow_id.clone();
        let mut current = started;
        let mut opened = None;
        loop {
            let own = flow.is_some() && current.flow_id == flow;
            if own {
                let _ = updates.send(Update::Auth(current.clone()));
                if let Some(Some(page)) = model::settle_page(Some(&current), flow.as_deref())
                    && opened.as_ref() != Some(&page)
                {
                    opened = Some(page.clone());
                    let _ = updates.send(Update::Page(page));
                }
                match current.phase.as_str() {
                    "succeeded" => break,
                    "failed" | "cancelled" => {
                        return Err(current.message.unwrap_or_else(|| "ChatGPT sign-in could not finish. Try again.".into()));
                    }
                    _ => {}
                }
            }
            current = auth::decode_auth_state(item(&mut stream).await?).ok_or("Could not read sign-in status.")?;
        }
    }
    let _ = updates.send(Update::Finishing);
    // The view applies this snapshot on its destination connection only.
    Ok(())
}
static NEXT_ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[cfg(test)]
mod tests;
