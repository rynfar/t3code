//! Connect screen account lifecycle, ported from web/cloud/managedAuth.tsx and relay discovery.
use crate::app_state::{AppEvent, AppState, Phase};
use gpui::Context;
use t3_core::relay::{Discovery, Environment};

pub struct CloudState {
    pub service: std::sync::Arc<t3_client::connect::Cloud>,
    pub identity: Option<String>,
    pub discovery: Discovery,
    pub busy: bool,
    pub signing_in: bool,
    pub error: Option<String>,
    generation: u64,
    task: Option<tokio::task::AbortHandle>,
}
impl Default for CloudState {
    fn default() -> Self {
        Self {
            service: t3_client::connect::Cloud::new(),
            identity: None,
            discovery: Default::default(),
            busy: false,
            signing_in: false,
            error: None,
            generation: 0,
            task: None,
        }
    }
}
impl AppState {
    pub fn refresh_cloud(&mut self, cx: &mut Context<Self>) {
        if self.cloud.busy {
            return;
        }
        self.cloud.busy = true;
        self.cloud.error = None;
        self.cloud.discovery = Discovery {
            refreshing: true,
            ..Default::default()
        };
        let (service, generation) = (self.cloud.service.clone(), self.cloud.generation);
        let task = self.runtime().spawn(async move {
            let identity = service.identity().await?;
            let discovery = if identity.is_some() {
                service.discover().await
            } else {
                Ok(Discovery::default())
            };
            Ok::<_, String>((identity, discovery))
        });
        self.cloud.task = Some(task.abort_handle());
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.busy = false;
                state.cloud.task = None;
                state.cloud.discovery.refreshing = false;
                match result {
                    Ok(Ok((identity, discovery))) => {
                        if identity.is_none() { state.clear_relay_environments(cx); }
                        state.cloud.identity = identity;
                        match discovery {
                            Ok(d) => state.cloud.discovery = d,
                            Err(error) => state.cloud.error = Some(error),
                        }
                    }
                    Ok(Err(error)) => state.cloud.error = Some(error),
                    Err(_) => state.cloud.error = Some("Could not load T3 Connect data.".into()),
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }
    pub fn sign_in_cloud(&mut self, cx: &mut Context<Self>) {
        if self.cloud.busy {
            return;
        }
        self.cloud.busy = true;
        self.cloud.signing_in = true;
        self.cloud.error = None;
        self.cloud.generation += 1;
        let generation = self.cloud.generation;
        let service = self.cloud.service.clone();
        let (browser, url) = tokio::sync::oneshot::channel();
        let task = self.runtime().spawn(async move { service.sign_in(browser).await });
        self.cloud.task = Some(task.abort_handle());
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            if let Ok(url) = url.await {
                let _ = this.update(cx, |state, cx| {
                    if state.cloud.generation == generation {
                        cx.open_url(&url);
                    }
                });
            }
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.busy = false;
                state.cloud.signing_in = false;
                state.cloud.task = None;
                match result {
                    Ok(Ok(identity)) => {
                        state.clear_relay_environments(cx);
                        state.cloud.identity = Some(identity);
                        state.refresh_cloud(cx);
                    }
                    Ok(Err(error)) => state.cloud.error = Some(error),
                    Err(_) => state.cloud.error = Some("T3 Connect sign-in was cancelled.".into()),
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }
    pub fn cancel_cloud_sign_in(&mut self, cx: &mut Context<Self>) {
        self.sign_out_cloud(cx);
    }
    fn cancel_cloud_task(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self.cloud.task.take() {
            task.abort();
        }
        self.cloud.generation += 1;
        self.cloud.busy = false;
        self.cloud.signing_in = false;
        cx.emit(AppEvent::Phase);
    }
    pub fn sign_out_cloud(&mut self, cx: &mut Context<Self>) {
        self.cloud.signing_in = false;
        self.cancel_cloud_task(cx);
        self.clear_relay_environments(cx);
        self.cloud.identity = None;
        self.cloud.discovery = Default::default();
        self.cloud.error = None;
        self.cloud.busy = true;
        let service = self.cloud.service.clone();
        let generation = self.cloud.generation;
        let task = self.runtime().spawn(async move { service.sign_out().await });
        cx.emit(AppEvent::Phase);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if state.cloud.generation != generation {
                    return;
                }
                state.cloud.busy = false;
                if let Ok(Err(error)) = result {
                    state.cloud.error = Some(error);
                }
                cx.emit(AppEvent::Phase);
            });
        })
        .detach();
    }
    fn clear_relay_environments(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<_> = self
            .environments
            .environments
            .iter()
            .filter(|e| e.relay.is_some())
            .map(|e| e.id.clone())
            .collect();
        let active = self.active_environment().is_some_and(|e| e.relay.is_some());
        for id in ids {
            self.environments.remove(&id);
        }
        self.persist_environments();
        if active {
            self.reset_connection(cx);
            self.connect.switching = false;
            self.set_phase(Phase::Unpaired, cx);
        }
    }
    pub fn connect_cloud_environment(&mut self, environment: Environment, cx: &mut Context<Self>) {
        if self.cloud.identity.is_none() || self.cloud.busy {
            return;
        }
        let id = self.environments.upsert_relay(environment);
        self.persist_environments();
        self.reset_connection(cx);
        self.connect.switching = false;
        self.start_environment(&id, cx);
    }
}
