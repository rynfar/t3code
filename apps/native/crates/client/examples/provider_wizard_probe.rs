//! Smoke-only round trip of the add wizard's Create payload, followed by exact settings restoration.
use serde_json::json;
use t3_client::{store, supervisor::start};
use t3_core::{
    commands::{self, method},
    settings::providers::{self as model, wizard},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::args().any(|arg| arg == "--allow-write"),
        "requires --allow-write"
    );
    anyhow::ensure!(
        std::env::var_os("T3_NATIVE_HOME").is_some(),
        "requires a copied T3_NATIVE_HOME"
    );
    let credentials = store::load().ok_or_else(|| anyhow::anyhow!("no copied smoke connection"))?;
    anyhow::ensure!(
        credentials.http_base.trim_end_matches('/') == "http://127.0.0.1:14658",
        "only smoke server 14658 is allowed"
    );
    let (client, mut events) = start(&tokio::runtime::Handle::current(), credentials);
    // Keep the supervisor's event queue drained while the raw subscription captures every settings key.
    tokio::spawn(async move { while events.recv().await.is_some() {} });
    let mut stream = client
        .stream(
            method::SUBSCRIBE_SERVER_CONFIG,
            commands::subscribe_server_config(),
        )
        .await?;
    let snapshot = stream
        .receiver
        .recv()
        .await
        .ok_or_else(|| anyhow::anyhow!("no config snapshot"))??;
    let before = snapshot["config"]["settings"].clone();
    anyhow::ensure!(before.is_object(), "config snapshot has no settings");
    let id = "native_wizard_disposable_probe";
    anyhow::ensure!(
        before["providerInstances"].get(id).is_none(),
        "disposable id already exists"
    );
    let instance = wizard::instance(
        "opencode",
        "Disposable wizard probe",
        "#ABCDEF",
        Some(json!({
            "binaryPath":"/nonexistent/t3-native-wizard-fixture",
            "serverUrl":"http://127.0.0.1:1",
            "serverPassword":"disposable-fixture-only",
            "futureWizardKey":{"keep":true}
        })),
        vec![],
    );
    let payload = model::create_payload(id, &instance);
    println!("server: http://127.0.0.1:14658; no auth, install or turn requests");
    println!(
        "send: server.updateSettings operation=create driver=opencode enabled=true; config=binaryPath,serverUrl,serverPassword,futureWizardKey"
    );
    let created = client
        .request(method::SERVER_UPDATE_SETTINGS, payload)
        .await;
    // Cleanup runs even if the create reply is an error; the command may have committed.
    let removed = client
        .request(method::SERVER_UPDATE_SETTINGS, model::remove_payload(id))
        .await?;
    anyhow::ensure!(
        removed == before,
        "settings after remove differ from the complete pre-create snapshot"
    );
    println!(
        "remove: disposable instance gone; complete raw settings equal the pre-create snapshot"
    );
    let created = created?;
    let saved = &created["providerInstances"][id];
    anyhow::ensure!(
        saved["config"]["binaryPath"] == "/nonexistent/t3-native-wizard-fixture",
        "binary path not saved"
    );
    anyhow::ensure!(
        saved["config"]["futureWizardKey"] == json!({"keep":true}),
        "unknown config lost"
    );
    anyhow::ensure!(
        saved["enabled"] == true,
        "enabled flag differs from wizard payload"
    );
    anyhow::ensure!(
        saved["displayName"] == "Disposable wizard probe" && saved["accentColor"] == "#ABCDEF",
        "identity differs from wizard payload"
    );
    anyhow::ensure!(
        saved["config"]["serverPassword"] == "••••••",
        "saved secret marker is missing"
    );
    println!(
        "create reply: schema field, unknown key, identity and enabled flag persisted; secret redacted={}",
        saved["config"]["serverPassword"] != "disposable-fixture-only"
    );
    Ok(())
}
