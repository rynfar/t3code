//! RFC 9449 DPoP device key, proof signing/verification, and direct-connection credential store.
//! Ports `packages/shared/src/dpopCommon.ts`, `packages/shared/src/dpop.ts`,
//! `packages/client-runtime/src/relay/dpopProof.ts`, and
//! `packages/client-runtime/src/connection/{catalog,credentialStore}.ts` (`pingdotgg/t3code#37980`,
//! `pingdotgg/t3code#37990`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::digest::{SHA256, digest};
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair,
    UnparsedPublicKey,
};
use serde::{Deserialize, Serialize};
use url::Url;

pub const DPOP_TYP: &str = "dpop+jwt";
pub const DPOP_ALG: &str = "ES256";
/// How far a proof's `iat` may sit from the verifier's clock, in either direction.
pub const DPOP_MAX_CLOCK_SKEW_SECONDS: i64 = 300;
/// `DPOP_ACCESS_TOKEN_REFRESH_SKEW_MS` (`packages/client-runtime/src/connection/model.ts`).
pub const DPOP_ACCESS_TOKEN_REFRESH_SKEW_MS: i64 = 60_000;

const DPOP_KEY_FILE: &str = "dpop-key.pkcs8";
const DPOP_CREDENTIALS_FILE: &str = "dpop-credentials.json";

/// `DpopPublicJwk` (`packages/shared/src/dpopCommon.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpopPublicJwk {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub y: String,
}

/// `DpopConnectionCredential` (`packages/client-runtime/src/connection/catalog.ts`):
/// a token bound to this device's DPoP key, which the host renews while the device uses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DpopConnectionCredential {
    pub access_token: String,
    pub expires_at_epoch_ms: i64,
    /// Original access token returned when this host paired, kept so callers holding the saved
    /// environment's initial token (across renewals, alternate routes, or SSH port relocations)
    /// still resolve this DPoP credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_token: Option<String>,
    /// Immediately superseded access token, so concurrent in-flight requests resolve cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
}

impl DpopConnectionCredential {
    pub fn new(access_token: String, expires_at_epoch_ms: i64, environment_id: Option<String>) -> Self {
        Self {
            seed_token: Some(access_token.clone()),
            previous_access_token: None,
            environment_id,
            access_token,
            expires_at_epoch_ms,
        }
    }

    /// Whether `token` identifies this credential (current, initial saved seed, or just-replaced).
    pub fn matches_token(&self, token: &str) -> bool {
        !token.is_empty()
            && (self.access_token == token
                || self.seed_token.as_deref() == Some(token)
                || self.previous_access_token.as_deref() == Some(token))
    }

    /// Returns a renewed copy preserving `seed_token` and recording the replaced token.
    pub fn renewed(&self, access_token: String, expires_at_epoch_ms: i64) -> Self {
        Self {
            seed_token: self.seed_token.clone().or_else(|| Some(self.access_token.clone())),
            previous_access_token: Some(self.access_token.clone()),
            environment_id: self.environment_id.clone(),
            access_token,
            expires_at_epoch_ms,
        }
    }
}

/// `ConnectionCredential` (`packages/client-runtime/src/connection/catalog.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum ConnectionCredential {
    #[serde(rename = "BearerConnectionCredential")]
    Bearer { token: String },
    #[serde(rename = "DpopConnectionCredential")]
    Dpop(DpopConnectionCredential),
}

/// `normalizeDpopHtu` (`packages/shared/src/dpopCommon.ts`): strips query and fragment.
pub fn normalize_dpop_htu(url: &str) -> Option<String> {
    let mut parsed = Url::parse(url).ok()?;
    parsed.set_fragment(None);
    parsed.set_query(None);
    Some(parsed.into())
}

/// Maps a WebSocket URL (`ws://` / `wss://`) to the HTTP(S) `htu` the server verifies on upgrade
/// (`packages/client-runtime/src/authorization/remote.ts`, `pingdotgg/t3code#37990`).
pub fn ws_url_to_http_htu(ws_url: &Url) -> Option<String> {
    let mut http_url = ws_url.clone();
    match http_url.scheme() {
        "wss" => http_url.set_scheme("https").ok()?,
        "ws" => http_url.set_scheme("http").ok()?,
        "https" | "http" => {}
        _ => return None,
    }
    http_url.set_fragment(None);
    http_url.set_query(None);
    Some(http_url.into())
}

/// RFC 7638 JWK thumbprint: SHA-256 over required EC members in lexicographic order
/// (`computeDpopJwkThumbprint` in `packages/shared/src/dpop.ts`).
pub fn compute_dpop_jwk_thumbprint(jwk: &DpopPublicJwk) -> String {
    // Members in strict RFC 7638 lexicographic order: crv, kty, x, y.
    #[derive(Serialize)]
    struct ThumbprintInput<'a> {
        crv: &'a str,
        kty: &'a str,
        x: &'a str,
        y: &'a str,
    }
    let canonical = serde_json::to_string(&ThumbprintInput {
        crv: &jwk.crv,
        kty: &jwk.kty,
        x: &jwk.x,
        y: &jwk.y,
    })
    .expect("thumbprint JSON serializes");
    sha256_base64url(canonical.as_bytes())
}

/// `computeDpopAccessTokenHash` (`packages/shared/src/dpop.ts`): base64url(SHA-256(access_token)).
pub fn compute_dpop_access_token_hash(access_token: &str) -> String {
    sha256_base64url(access_token.as_bytes())
}

fn sha256_base64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(digest(&SHA256, bytes).as_ref())
}

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn jwk_from_uncompressed_public_key(bytes: &[u8]) -> Result<DpopPublicJwk, String> {
    if bytes.len() != 65 || bytes[0] != 0x04 {
        return Err("Invalid P-256 uncompressed public key.".to_owned());
    }
    Ok(DpopPublicJwk {
        kty: "EC".to_owned(),
        crv: "P-256".to_owned(),
        x: URL_SAFE_NO_PAD.encode(&bytes[1..33]),
        y: URL_SAFE_NO_PAD.encode(&bytes[33..65]),
    })
}

fn uncompressed_public_key_from_jwk(jwk: &DpopPublicJwk) -> Result<[u8; 65], String> {
    if jwk.kty != "EC" || jwk.crv != "P-256" {
        return Err("Unsupported JWK key type or curve.".to_owned());
    }
    let x = URL_SAFE_NO_PAD.decode(&jwk.x).map_err(|_| "Invalid JWK x coordinate.".to_owned())?;
    let y = URL_SAFE_NO_PAD.decode(&jwk.y).map_err(|_| "Invalid JWK y coordinate.".to_owned())?;
    if x.len() != 32 || y.len() != 32 {
        return Err("Invalid P-256 public key coordinate length.".to_owned());
    }
    let mut out = [0u8; 65];
    out[0] = 0x04;
    out[1..33].copy_from_slice(&x);
    out[33..65].copy_from_slice(&y);
    Ok(out)
}

/// A P-256 `ES256` device proof key backed by `ring::signature::EcdsaKeyPair`.
pub struct DpopDeviceKey {
    key_pair: EcdsaKeyPair,
    public_jwk: DpopPublicJwk,
    thumbprint: String,
    pkcs8: Vec<u8>,
}

impl std::fmt::Debug for DpopDeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DpopDeviceKey")
            .field("thumbprint", &self.thumbprint)
            .finish_non_exhaustive()
    }
}

impl DpopDeviceKey {
    /// Generates a fresh P-256 PKCS#8 keypair.
    pub fn generate() -> Result<Self, String> {
        let rng = SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| "Could not generate P-256 DPoP key.".to_owned())?;
        Self::from_pkcs8(doc.as_ref())
    }

    /// Loads a P-256 keypair from PKCS#8 DER bytes.
    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<Self, String> {
        let rng = SystemRandom::new();
        let key_pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, &rng)
            .map_err(|_| "Could not parse stored P-256 DPoP key.".to_owned())?;
        let public_jwk = jwk_from_uncompressed_public_key(key_pair.public_key().as_ref())?;
        let thumbprint = compute_dpop_jwk_thumbprint(&public_jwk);
        Ok(Self {
            key_pair,
            public_jwk,
            thumbprint,
            pkcs8: pkcs8.to_vec(),
        })
    }

    /// Loads `<base>/dpop-key.pkcs8` or generates and persists a new key privately (`0o600`).
    pub fn load_or_create_in(base: &Path) -> Result<Self, String> {
        let path = base.join(DPOP_KEY_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => match Self::from_pkcs8(&bytes) {
                Ok(key) => return Ok(key),
                Err(_) => {
                    tracing::warn!(path = %path.display(), "stored DPoP key unreadable; generating a new key");
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Could not read DPoP key: {error}")),
        }
        let generated = Self::generate()?;
        write_private_bytes(base, DPOP_KEY_FILE, &generated.pkcs8)
            .map_err(|error| format!("Could not store DPoP key: {error}"))?;
        Ok(generated)
    }

    pub fn public_jwk(&self) -> &DpopPublicJwk {
        &self.public_jwk
    }

    pub fn thumbprint(&self) -> &str {
        &self.thumbprint
    }

    pub fn pkcs8_bytes(&self) -> &[u8] {
        &self.pkcs8
    }

    /// Creates an RFC 9449 `typ: "dpop+jwt"` / `alg: "ES256"` proof for `(method, url, access_token)`.
    pub fn create_proof(
        &self,
        method: &str,
        url: &str,
        access_token: Option<&str>,
    ) -> Result<String, String> {
        self.create_proof_at(method, url, access_token, now_epoch_secs())
    }

    /// Deterministic-timestamp variant of `create_proof`.
    pub fn create_proof_at(
        &self,
        method: &str,
        url: &str,
        access_token: Option<&str>,
        iat_epoch_secs: i64,
    ) -> Result<String, String> {
        let htu = normalize_dpop_htu(url).ok_or_else(|| "DPoP URL is invalid.".to_owned())?;
        #[derive(Serialize)]
        struct Header<'a> {
            typ: &'static str,
            alg: &'static str,
            jwk: &'a DpopPublicJwk,
        }
        #[derive(Serialize)]
        struct Payload<'a> {
            htm: String,
            htu: String,
            jti: String,
            iat: i64,
            #[serde(skip_serializing_if = "Option::is_none")]
            ath: Option<String>,
            #[serde(skip)]
            _marker: std::marker::PhantomData<&'a ()>,
        }
        let header_json = serde_json::to_vec(&Header {
            typ: DPOP_TYP,
            alg: DPOP_ALG,
            jwk: &self.public_jwk,
        })
        .map_err(|error| error.to_string())?;
        let payload_json = serde_json::to_vec(&Payload {
            htm: method.to_ascii_uppercase(),
            htu,
            jti: uuid::Uuid::new_v4().to_string(),
            iat: iat_epoch_secs,
            ath: access_token.filter(|token| !token.is_empty()).map(compute_dpop_access_token_hash),
            _marker: std::marker::PhantomData,
        })
        .map_err(|error| error.to_string())?;
        let header_b64 = URL_SAFE_NO_PAD.encode(header_json);
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json);
        let signing_input = format!("{header_b64}.{payload_b64}");
        let rng = SystemRandom::new();
        let signature = self
            .key_pair
            .sign(&rng, signing_input.as_bytes())
            .map_err(|_| "Could not sign DPoP proof.".to_owned())?;
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.as_ref());
        Ok(format!("{signing_input}.{sig_b64}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDpopProof {
    pub thumbprint: String,
    pub jti: String,
    pub iat: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpopVerifyError {
    MissingProof,
    MalformedProof,
    KeyMismatch,
    MethodMismatch,
    UrlMismatch,
    AccessTokenHashMismatch,
    TimeWindow,
    InvalidSignature,
}

/// Verifies a compact `dpop+jwt` proof (`verifyDpopProof` in `packages/shared/src/dpop.ts`).
pub fn verify_dpop_proof(
    proof: &str,
    method: &str,
    url: &str,
    now_secs: i64,
    expected_thumbprint: Option<&str>,
    expected_access_token: Option<&str>,
    max_age_secs: Option<i64>,
) -> Result<VerifiedDpopProof, DpopVerifyError> {
    if proof.trim().is_empty() {
        return Err(DpopVerifyError::MissingProof);
    }
    let parts: Vec<&str> = proof.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(DpopVerifyError::MalformedProof);
    }
    #[derive(Deserialize)]
    struct Header {
        typ: String,
        alg: String,
        jwk: serde_json::Value,
    }
    #[derive(Deserialize)]
    struct Payload {
        htm: String,
        htu: String,
        jti: String,
        iat: i64,
        #[serde(default)]
        ath: Option<String>,
    }
    let header_bytes = URL_SAFE_NO_PAD.decode(parts[0]).map_err(|_| DpopVerifyError::MalformedProof)?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(parts[1]).map_err(|_| DpopVerifyError::MalformedProof)?;
    let header: Header = serde_json::from_slice(&header_bytes).map_err(|_| DpopVerifyError::MalformedProof)?;
    let payload: Payload = serde_json::from_slice(&payload_bytes).map_err(|_| DpopVerifyError::MalformedProof)?;
    if header.typ != DPOP_TYP || header.alg != DPOP_ALG || header.jwk.get("d").is_some() {
        return Err(DpopVerifyError::MalformedProof);
    }
    let jwk: DpopPublicJwk = serde_json::from_value(header.jwk).map_err(|_| DpopVerifyError::MalformedProof)?;
    let thumbprint = compute_dpop_jwk_thumbprint(&jwk);
    if expected_thumbprint.is_some_and(|wanted| wanted != thumbprint) {
        return Err(DpopVerifyError::KeyMismatch);
    }
    if !payload.htm.eq_ignore_ascii_case(method) {
        return Err(DpopVerifyError::MethodMismatch);
    }
    let normalized_htu = normalize_dpop_htu(url).ok_or(DpopVerifyError::UrlMismatch)?;
    if payload.htu != normalized_htu {
        return Err(DpopVerifyError::UrlMismatch);
    }
    if let Some(token) = expected_access_token {
        let expected_ath = compute_dpop_access_token_hash(token);
        if payload.ath.as_deref() != Some(expected_ath.as_str()) {
            return Err(DpopVerifyError::AccessTokenHashMismatch);
        }
    }
    let signature = URL_SAFE_NO_PAD.decode(parts[2]).map_err(|_| DpopVerifyError::MalformedProof)?;
    let pub_bytes = uncompressed_public_key_from_jwk(&jwk).map_err(|_| DpopVerifyError::MalformedProof)?;
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, pub_bytes)
        .verify(signing_input.as_bytes(), &signature)
        .map_err(|_| DpopVerifyError::InvalidSignature)?;
    let max_age = max_age_secs.unwrap_or(DPOP_MAX_CLOCK_SKEW_SECONDS);
    if (now_secs - payload.iat).abs() > max_age {
        return Err(DpopVerifyError::TimeWindow);
    }
    Ok(VerifiedDpopProof {
        thumbprint,
        jti: payload.jti,
        iat: payload.iat,
    })
}

/// Process-wide cache of loaded device keys keyed by native home directory.
pub fn device_key() -> Result<Arc<DpopDeviceKey>, String> {
    let base = crate::store::base();
    static KEYS: OnceLock<Mutex<HashMap<Option<PathBuf>, Arc<DpopDeviceKey>>>> = OnceLock::new();
    let cache = KEYS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(key) = guard.get(&base) {
        return Ok(key.clone());
    }
    let loaded = match &base {
        Some(dir) => DpopDeviceKey::load_or_create_in(dir)?,
        None => DpopDeviceKey::generate()?,
    };
    let arc = Arc::new(loaded);
    guard.insert(base, arc.clone());
    Ok(arc)
}

fn normalize_host_key(http_base: &str) -> String {
    http_base.trim().trim_end_matches('/').to_owned()
}

type HostCredentialMap = HashMap<String, DpopConnectionCredential>;
type CredentialStoreMap = HashMap<Option<PathBuf>, HostCredentialMap>;

fn credential_store_lock() -> &'static Mutex<CredentialStoreMap> {
    static STORE: OnceLock<Mutex<CredentialStoreMap>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn load_credentials_file(base: &Path) -> HashMap<String, DpopConnectionCredential> {
    match std::fs::read_to_string(base.join(DPOP_CREDENTIALS_FILE)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn save_credentials_file(base: &Path, map: &HashMap<String, DpopConnectionCredential>) {
    if let Ok(text) = serde_json::to_string_pretty(map)
        && let Err(error) = write_private_bytes(base, DPOP_CREDENTIALS_FILE, text.as_bytes())
    {
        tracing::warn!(%error, "could not persist DPoP credentials");
    }
}

/// Returns the stored direct DPoP credential for `http_base`, if that host paired with a DPoP key.
pub fn get_direct_credential(http_base: &str) -> Option<DpopConnectionCredential> {
    let key = normalize_host_key(http_base);
    let base = crate::store::base();
    let mut guard = credential_store_lock().lock().unwrap_or_else(|p| p.into_inner());
    let map = guard
        .entry(base.clone())
        .or_insert_with(|| base.as_deref().map(load_credentials_file).unwrap_or_default());
    map.get(&key).cloned()
}

/// Resolves the stored direct DPoP credential for `(http_base, access_token)` across route switches,
/// token renewals, and SSH/local port relocations (`pingdotgg/t3code#37990`).
///
/// Returns `(canonical_store_key, credential)` so renewals update the canonical entry and serialize
/// on its lock even when the active request targets an alternate route origin.
pub fn find_direct_credential(http_base: &str, access_token: &str) -> Option<(String, DpopConnectionCredential)> {
    let key = normalize_host_key(http_base);
    let base = crate::store::base();
    let mut guard = credential_store_lock().lock().unwrap_or_else(|p| p.into_inner());
    let map = guard
        .entry(base.clone())
        .or_insert_with(|| base.as_deref().map(load_credentials_file).unwrap_or_default());
    if let Some(cred) = map.get(&key)
        && (access_token.is_empty() || cred.matches_token(access_token))
    {
        return Some((key, cred.clone()));
    }
    if !access_token.is_empty()
        && let Some((found_key, cred)) = map.iter().find(|(_, cred)| cred.matches_token(access_token))
    {
        return Some((found_key.clone(), cred.clone()));
    }
    None
}

/// Persists a direct DPoP credential for `http_base` (and writes privately to `<home>/dpop-credentials.json`).
pub fn put_direct_credential(http_base: &str, mut credential: DpopConnectionCredential) {
    if credential.seed_token.is_none() {
        credential.seed_token = Some(credential.access_token.clone());
    }
    let key = normalize_host_key(http_base);
    let base = crate::store::base();
    let mut guard = credential_store_lock().lock().unwrap_or_else(|p| p.into_inner());
    let map = guard
        .entry(base.clone())
        .or_insert_with(|| base.as_deref().map(load_credentials_file).unwrap_or_default());
    map.insert(key, credential);
    if let Some(dir) = base.as_deref() {
        save_credentials_file(dir, map);
    }
}

/// Updates an existing canonical DPoP credential entry in place (e.g. after automatic renewal on
/// either the primary or an alternate route).
pub fn update_direct_credential(store_key: &str, mut credential: DpopConnectionCredential) {
    if credential.seed_token.is_none() {
        credential.seed_token = Some(credential.access_token.clone());
    }
    let key = normalize_host_key(store_key);
    let base = crate::store::base();
    let mut guard = credential_store_lock().lock().unwrap_or_else(|p| p.into_inner());
    let map = guard
        .entry(base.clone())
        .or_insert_with(|| base.as_deref().map(load_credentials_file).unwrap_or_default());
    map.insert(key, credential);
    if let Some(dir) = base.as_deref() {
        save_credentials_file(dir, map);
    }
}

/// Migrates a stored DPoP credential when a saved environment moves to a new address (e.g., an SSH
/// tunnel reserving a new ephemeral port or a local server restarting on a different port).
pub fn relocate_direct_credential(old_http_base: &str, new_http_base: &str, access_token: Option<&str>) {
    let old_key = normalize_host_key(old_http_base);
    let new_key = normalize_host_key(new_http_base);
    if old_key == new_key {
        return;
    }
    let base = crate::store::base();
    let mut guard = credential_store_lock().lock().unwrap_or_else(|p| p.into_inner());
    let map = guard
        .entry(base.clone())
        .or_insert_with(|| base.as_deref().map(load_credentials_file).unwrap_or_default());
    let existing = map.remove(&old_key).or_else(|| {
        let token = access_token.filter(|t| !t.is_empty())?;
        let found_key = map
            .iter()
            .find(|(_, cred)| cred.matches_token(token))
            .map(|(k, _)| k.clone())?;
        map.remove(&found_key)
    });
    if let Some(cred) = existing {
        map.insert(new_key, cred);
        if let Some(dir) = base.as_deref() {
            save_credentials_file(dir, map);
        }
    }
}

/// Removes any stored direct DPoP credential for `http_base` (e.g. when re-pairing as Bearer or revoking).
pub fn remove_direct_credential(http_base: &str) {
    remove_direct_credential_matching(http_base, None);
}

/// Removes any stored direct DPoP credential keyed by `http_base` or matching `access_token`.
pub fn remove_direct_credential_matching(http_base: &str, access_token: Option<&str>) {
    let key = normalize_host_key(http_base);
    let base = crate::store::base();
    let mut guard = credential_store_lock().lock().unwrap_or_else(|p| p.into_inner());
    let map = guard
        .entry(base.clone())
        .or_insert_with(|| base.as_deref().map(load_credentials_file).unwrap_or_default());
    let mut removed = map.remove(&key).is_some();
    if let Some(token) = access_token.filter(|t| !t.is_empty()) {
        let before = map.len();
        map.retain(|_, cred| !cred.matches_token(token));
        removed |= map.len() != before;
    }
    if removed && let Some(dir) = base.as_deref() {
        save_credentials_file(dir, map);
    }
}

/// Per-credential async renewal lock (`directTokenLocks` in `packages/client-runtime/src/authorization/service.ts`):
/// requests for the same host share one renewal without waiting on other hosts.
pub fn direct_renewal_lock(store_key: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let key = normalize_host_key(store_key);
    locks
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(key)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn write_private_bytes(base: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(base)?;
    let temporary = base.join(format!(".{name}.{}.{}.tmp", std::process::id(), uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options
        .open(&temporary)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| std::fs::rename(&temporary, base.join(name)));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_persists_reloads_p256_key_and_signs_verifiable_dpop_proofs() {
        let dir = std::env::temp_dir().join(format!("t3-dpop-key-{}", uuid::Uuid::new_v4()));
        let first = DpopDeviceKey::load_or_create_in(&dir).expect("generate key");
        let second = DpopDeviceKey::load_or_create_in(&dir).expect("reload key");
        assert_eq!(first.public_jwk(), second.public_jwk());
        assert_eq!(first.thumbprint(), second.thumbprint());
        assert_eq!(first.public_jwk().kty, "EC");
        assert_eq!(first.public_jwk().crv, "P-256");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(DPOP_KEY_FILE)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let now = 1_793_000_000i64;
        let url = "https://studio.ts.net:3773/api/auth/websocket-ticket/rpc?foo=bar#frag";
        let proof = first
            .create_proof_at("post", url, Some("access-tok-123"), now)
            .expect("create proof");
        let verified = verify_dpop_proof(
            &proof,
            "POST",
            "https://studio.ts.net:3773/api/auth/websocket-ticket/rpc",
            now + 5,
            Some(first.thumbprint()),
            Some("access-tok-123"),
            None,
        )
        .expect("proof verifies");
        assert_eq!(verified.thumbprint, first.thumbprint());
        assert_eq!(verified.iat, now);

        // Method, URL, ath, thumbprint, and clock-window checks reject mismatches.
        assert_eq!(
            verify_dpop_proof(&proof, "GET", url, now, None, None, None),
            Err(DpopVerifyError::MethodMismatch)
        );
        assert_eq!(
            verify_dpop_proof(&proof, "POST", "https://other.ts.net/api/auth/websocket-ticket/rpc", now, None, None, None),
            Err(DpopVerifyError::UrlMismatch)
        );
        assert_eq!(
            verify_dpop_proof(&proof, "POST", url, now, None, Some("wrong-token"), None),
            Err(DpopVerifyError::AccessTokenHashMismatch)
        );
        assert_eq!(
            verify_dpop_proof(&proof, "POST", url, now, Some("wrong-thumbprint"), None, None),
            Err(DpopVerifyError::KeyMismatch)
        );
        assert_eq!(
            verify_dpop_proof(&proof, "POST", url, now + DPOP_MAX_CLOCK_SKEW_SECONDS + 1, None, None, None),
            Err(DpopVerifyError::TimeWindow)
        );

        // Tampering with signature fails verification.
        let mut tampered: Vec<&str> = proof.split('.').collect();
        let mut bad_sig = URL_SAFE_NO_PAD.decode(tampered[2]).unwrap();
        bad_sig[0] ^= 0xff;
        let bad_sig_b64 = URL_SAFE_NO_PAD.encode(bad_sig);
        tampered[2] = &bad_sig_b64;
        let bad_proof = tampered.join(".");
        assert_eq!(
            verify_dpop_proof(&bad_proof, "POST", url, now, None, None, None),
            Err(DpopVerifyError::InvalidSignature)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normalize_htu_and_rfc7638_thumbprint_are_canonical() {
        assert_eq!(
            normalize_dpop_htu("https://host.example:3773/oauth/token?a=1#frag").as_deref(),
            Some("https://host.example:3773/oauth/token")
        );
        assert_eq!(normalize_dpop_htu("not a url"), None);
        let jwk = DpopPublicJwk {
            kty: "EC".into(),
            crv: "P-256".into(),
            x: "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU".into(),
            y: "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0".into(),
        };
        // Verified RFC 7638 canonical JSON SHA-256 base64url.
        let expected = sha256_base64url(
            br#"{"crv":"P-256","kty":"EC","x":"f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU","y":"x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0"}"#,
        );
        assert_eq!(compute_dpop_jwk_thumbprint(&jwk), expected);

        let wss = Url::parse("wss://studio.ts.net:3773/ws?wsTicket=abc#frag").unwrap();
        assert_eq!(ws_url_to_http_htu(&wss).as_deref(), Some("https://studio.ts.net:3773/ws"));
        let ws = Url::parse("ws://127.0.0.1:3773/ws?wsTicket=abc").unwrap();
        assert_eq!(ws_url_to_http_htu(&ws).as_deref(), Some("http://127.0.0.1:3773/ws"));
    }

    #[test]
    fn dpop_store_survives_route_switch_renewal_and_relocation() {
        let initial_base = format!("http://127.0.0.1:{}", 49000 + (uuid::Uuid::new_v4().as_u128() % 9000));
        let fallback_base = format!("https://studio-{}.ts.net:3773", uuid::Uuid::new_v4().simple());
        let relocated_base = format!("http://127.0.0.1:{}", 58000 + (uuid::Uuid::new_v4().as_u128() % 5000));
        let seed = format!("seed-tok-{}", uuid::Uuid::new_v4().simple());
        let renewed_tok = format!("renewed-tok-{}", uuid::Uuid::new_v4().simple());

        let initial = DpopConnectionCredential::new(seed.clone(), 1_900_000_000_000, Some("env-1".into()));
        put_direct_credential(&initial_base, initial);

        // Alternate route resolves by seed token.
        let (key, found) = find_direct_credential(&fallback_base, &seed).expect("alternate route matches seed");
        assert_eq!(key, initial_base);
        assert_eq!(found.access_token, seed);

        // Renewing updates the canonical entry while keeping the initial seed token matchable.
        let next = found.renewed(renewed_tok.clone(), 2_000_000_000_000);
        update_direct_credential(&key, next);
        let (_, via_seed) = find_direct_credential(&fallback_base, &seed).expect("stale seed resolves renewed");
        assert_eq!(via_seed.access_token, renewed_tok);

        // Unrelated Bearer token on the same origin must not inherit the DPoP credential.
        assert!(find_direct_credential(&initial_base, "unrelated-bearer-token").is_none());

        // SSH/local relocation migrates the key cleanly without leaving an orphan.
        relocate_direct_credential(&initial_base, &relocated_base, Some(&seed));
        assert!(get_direct_credential(&initial_base).is_none());
        let (relocated_key, relocated) =
            find_direct_credential(&relocated_base, &seed).expect("relocated key resolves");
        assert_eq!(relocated_key, relocated_base);
        assert_eq!(relocated.access_token, renewed_tok);

        // Removal by matching token cleans up regardless of base.
        remove_direct_credential_matching(&initial_base, Some(&seed));
        assert!(find_direct_credential(&relocated_base, &seed).is_none());
    }
}
