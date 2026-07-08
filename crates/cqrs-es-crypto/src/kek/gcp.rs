//! Google Cloud KMS-backed [`KekProvider`] (cargo feature `gcp-kms`).
//!
//! The KEK is a KMS `ENCRYPT_DECRYPT` crypto key that **never leaves KMS**: DEKs
//! are wrapped/unwrapped by calling the KMS REST API `:encrypt`/`:decrypt`. An
//! access token is obtained from the GCE/Cloud Run metadata server. All HTTP uses
//! the crate's rustls `reqwest`, so the single-TLS-stack / distroless invariant
//! is preserved.
//!
//! The `kek_id` is version-agnostic (`gcp-kms:{crypto-key-id}`): GCP symmetric
//! `decrypt` auto-selects the key version from the ciphertext, so KMS
//! auto-rotation needs no application re-wrap (old versions must stay ENABLED).
//! Each DEK is bound to its `key_id` via KMS `additionalAuthenticatedData`.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;
use zeroize::Zeroizing;

use crate::{
    cipher::KeyMaterial,
    kek::{KekError, KekHandle, KekProvider, WrappedDek},
};

const METADATA_TOKEN_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";
const KMS_ENDPOINT: &str = "https://cloudkms.googleapis.com";
/// Refresh the token this long before its stated expiry.
const TOKEN_SKEW: Duration = Duration::from_secs(60);

// ── Access token from the metadata server ────────────────────────────────────

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

struct CachedToken {
    token: String,
    expires_at: Instant,
}

/// Fetches and caches a service-account access token from the metadata server.
pub struct TokenSource {
    http: reqwest::Client,
    metadata_url: String,
    cached: RwLock<Option<CachedToken>>,
}

impl TokenSource {
    #[must_use]
    fn new(http: reqwest::Client, metadata_url: String) -> Self {
        Self {
            http,
            metadata_url,
            cached: RwLock::new(None),
        }
    }

    /// A valid access token, from cache when fresh.
    async fn token(&self) -> Result<String, KekError> {
        {
            let guard = self.cached.read().await;
            if let Some(t) = guard.as_ref() {
                if t.expires_at > Instant::now() {
                    return Ok(t.token.clone());
                }
            }
        }
        self.fetch().await
    }

    /// Force a refresh (used after a 401).
    async fn force_refresh(&self) -> Result<String, KekError> {
        self.fetch().await
    }

    async fn fetch(&self) -> Result<String, KekError> {
        // Fetch outside the lock (don't hold the write guard across the network
        // await). A concurrent cold fetch just does a redundant, idempotent
        // metadata request — cheap and rare.
        let resp = self
            .http
            .get(&self.metadata_url)
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .map_err(|e| KekError::Transport(e.to_string().into()))?;
        if !resp.status().is_success() {
            return Err(KekError::Transport(
                format!("metadata token request returned HTTP {}", resp.status()).into(),
            ));
        }
        let body: TokenResponse = resp
            .json()
            .await
            .map_err(|e| KekError::Transport(e.to_string().into()))?;
        let ttl = Duration::from_secs(body.expires_in).saturating_sub(TOKEN_SKEW);
        let token = body.access_token;
        {
            let mut guard = self.cached.write().await;
            *guard = Some(CachedToken {
                token: token.clone(),
                expires_at: Instant::now() + ttl,
            });
        }
        Ok(token)
    }
}

// ── KMS provider ─────────────────────────────────────────────────────────────

/// Classifies a KMS HTTP call outcome so wrap/unwrap can map to the right
/// [`KekError`] variant (`Wrap` vs `Unwrap`) while sharing transport handling.
enum KmsCallError {
    /// Retriable class — network, metadata, 5xx, 429.
    Transport(String),
    /// A definitive KMS/HTTP failure (4xx other than 429) or malformed response.
    Failed(String),
}

/// [`KekProvider`] that wraps/unwraps DEKs with a Cloud KMS crypto key.
pub struct GcpKmsKekProvider {
    /// Full resource id: `projects/{p}/locations/{l}/keyRings/{r}/cryptoKeys/{k}`.
    key_name: String,
    /// Stable logical id stored in `WrappedDek::kek_id`: `gcp-kms:{k}`.
    kek_id: String,
    http: reqwest::Client,
    tokens: TokenSource,
    endpoint: String,
}

impl GcpKmsKekProvider {
    /// Build from the crypto-key resource id (`HR_KEK_KMS_KEY`). Sync, no I/O.
    ///
    /// # Errors
    /// Returns [`KekError::Wrap`] if `key_name` is not a valid crypto-key path.
    pub fn new(key_name: impl Into<String>) -> Result<Self, KekError> {
        let key_name = key_name.into();
        let short = key_name
            .rsplit('/')
            .find(|s| !s.is_empty())
            .ok_or_else(|| KekError::Wrap(format!("invalid KMS key name: {key_name}").into()))?;
        let kek_id = format!("gcp-kms:{short}");
        let http = reqwest::Client::new();
        let tokens = TokenSource::new(http.clone(), METADATA_TOKEN_URL.to_string());
        Ok(Self {
            key_name,
            kek_id,
            http,
            tokens,
            endpoint: KMS_ENDPOINT.to_string(),
        })
    }

    async fn call(
        &self,
        op: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, KmsCallError> {
        kms_call(
            &self.http,
            &self.tokens,
            &self.endpoint,
            &self.key_name,
            op,
            body,
        )
        .await
    }
}

/// One KMS REST call (`{endpoint}/v1/{resource}:{op}`) with bearer auth, a
/// single retry on transport/401/5xx/429, and JSON parsing. Shared by the KEK
/// wrap/unwrap path and the blind-index `macSign` path.
async fn kms_call(
    http: &reqwest::Client,
    tokens: &TokenSource,
    endpoint: &str,
    resource: &str,
    op: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, KmsCallError> {
    let url = format!("{endpoint}/v1/{resource}:{op}");
    for attempt in 0u8..2 {
        let token = if attempt == 0 {
            tokens.token().await
        } else {
            tokens.force_refresh().await
        }
        .map_err(|e| KmsCallError::Transport(e.to_string()))?;

        let sent = http.post(&url).bearer_auth(&token).json(body).send().await;
        let resp = match sent {
            Ok(r) => r,
            Err(e) if attempt == 0 => {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let _ = e;
                continue;
            }
            Err(e) => return Err(KmsCallError::Transport(e.to_string())),
        };

        let status = resp.status();
        // 401 → token likely stale; refresh and retry once.
        if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            continue;
        }
        let retriable =
            status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
        if retriable && attempt == 0 {
            tokio::time::sleep(Duration::from_millis(150)).await;
            continue;
        }
        let body_text = resp
            .text()
            .await
            .map_err(|e| KmsCallError::Transport(e.to_string()))?;
        if !status.is_success() {
            let msg = format!("KMS {op} returned HTTP {status}: {body_text}");
            return Err(if retriable {
                KmsCallError::Transport(msg)
            } else {
                KmsCallError::Failed(msg)
            });
        }
        return serde_json::from_str(&body_text)
            .map_err(|e| KmsCallError::Failed(format!("KMS {op} response parse error: {e}")));
    }
    Err(KmsCallError::Transport(format!(
        "KMS {op} failed after retry"
    )))
}

/// KMS `macSign`-backed HMAC signer for [blind indexing](crate::blind_index).
///
/// Reuses the same token source and call/retry plumbing as the KEK provider,
/// but targets a **versioned** MAC key resource
/// (`projects/…/cryptoKeys/{k}/cryptoKeyVersions/{v}`) — `macSign` operates on a
/// specific key version, unlike symmetric `encrypt`/`decrypt`.
#[cfg(feature = "blind-index")]
pub(crate) struct KmsMacSigner {
    resource: String,
    http: reqwest::Client,
    tokens: TokenSource,
    endpoint: String,
}

#[cfg(feature = "blind-index")]
impl KmsMacSigner {
    pub(crate) fn new(resource: String) -> Self {
        let http = reqwest::Client::new();
        let tokens = TokenSource::new(http.clone(), METADATA_TOKEN_URL.to_string());
        Self {
            resource,
            http,
            tokens,
            endpoint: KMS_ENDPOINT.to_string(),
        }
    }

    /// HMAC `data` with the KMS-held key, returning the 32-byte digest.
    pub(crate) async fn mac_sign(&self, data: &[u8]) -> Result<[u8; 32], String> {
        let body = json!({ "data": B64.encode(data) });
        let resp = kms_call(
            &self.http,
            &self.tokens,
            &self.endpoint,
            &self.resource,
            "macSign",
            &body,
        )
        .await
        .map_err(|e| match e {
            KmsCallError::Transport(m) | KmsCallError::Failed(m) => m,
        })?;
        let mac = resp
            .get("mac")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "KMS macSign: missing mac".to_string())?;
        let bytes = B64
            .decode(mac)
            .map_err(|e| format!("KMS macSign: bad base64: {e}"))?;
        let len = bytes.len();
        <[u8; 32]>::try_from(bytes.as_slice())
            .map_err(|_| format!("KMS macSign: expected 32-byte HMAC-SHA256, got {len}"))
    }
}

#[async_trait]
impl KekProvider for GcpKmsKekProvider {
    fn current(&self) -> KekHandle {
        KekHandle {
            id: self.kek_id.clone(),
        }
    }

    fn by_id(&self, id: &str) -> Option<KekHandle> {
        (id == self.kek_id).then(|| KekHandle {
            id: self.kek_id.clone(),
        })
    }

    async fn wrap(&self, kek: &KekHandle, dek: &KeyMaterial) -> Result<WrappedDek, KekError> {
        if kek.id != self.kek_id {
            return Err(KekError::UnknownVersion(kek.id.clone()));
        }
        let body = json!({
            "plaintext": B64.encode(dek.key.as_slice()),
            "additionalAuthenticatedData": B64.encode(dek.key_id.as_bytes()),
        });
        let resp = self.call("encrypt", &body).await.map_err(|e| match e {
            KmsCallError::Transport(m) => KekError::Transport(m.into()),
            KmsCallError::Failed(m) => KekError::Wrap(m.into()),
        })?;
        let ciphertext = resp
            .get("ciphertext")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| KekError::Wrap("KMS encrypt: missing ciphertext".into()))?;
        let wrapped_key = B64
            .decode(ciphertext)
            .map_err(|e| KekError::Wrap(format!("KMS encrypt: bad base64: {e}").into()))?;
        Ok(WrappedDek {
            key_id: dek.key_id,
            kek_id: self.kek_id.clone(),
            wrapped_key,
        })
    }

    async fn unwrap(&self, wrapped: &WrappedDek) -> Result<KeyMaterial, KekError> {
        let body = json!({
            "ciphertext": B64.encode(&wrapped.wrapped_key),
            "additionalAuthenticatedData": B64.encode(wrapped.key_id.as_bytes()),
        });
        let resp = self.call("decrypt", &body).await.map_err(|e| match e {
            KmsCallError::Transport(m) => KekError::Transport(m.into()),
            KmsCallError::Failed(m) => KekError::Unwrap(m.into()),
        })?;
        let plaintext = resp
            .get("plaintext")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| KekError::Unwrap("KMS decrypt: missing plaintext".into()))?;
        let key = B64
            .decode(plaintext)
            .map_err(|e| KekError::Unwrap(format!("KMS decrypt: bad base64: {e}").into()))?;
        if key.len() != 32 {
            return Err(KekError::Unwrap(
                format!("KMS decrypt: expected 32-byte DEK, got {}", key.len()).into(),
            ));
        }
        Ok(KeyMaterial {
            key_id: wrapped.key_id,
            key: Zeroizing::new(key),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    // A fake KMS: :encrypt echoes the plaintext back as the ciphertext (base64),
    // :decrypt echoes it back — enough to prove request shape + round-trip wiring.
    async fn fake_kms() -> MockServer {
        MockServer::start().await
    }

    fn provider(server: &MockServer) -> GcpKmsKekProvider {
        let http = reqwest::Client::new();
        GcpKmsKekProvider {
            key_name: "projects/p/locations/l/keyRings/r/cryptoKeys/hr-dek-wrap".to_string(),
            kek_id: "gcp-kms:hr-dek-wrap".to_string(),
            http: http.clone(),
            tokens: TokenSource::new(http, format!("{}/token", server.uri())),
            endpoint: server.uri(),
        }
    }

    async fn mount_token(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/token"))
            .and(header("Metadata-Flavor", "Google"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fake-token", "expires_in": 3600, "token_type": "Bearer"
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn kek_id_is_derived_and_version_agnostic() {
        let server = fake_kms().await;
        let p = provider(&server);
        assert_eq!(p.current().id, "gcp-kms:hr-dek-wrap");
        assert!(p.by_id("gcp-kms:hr-dek-wrap").is_some());
        assert!(p.by_id("hr:v1").is_none());
    }

    #[tokio::test]
    async fn wrap_sends_plaintext_and_aad_with_bearer() {
        let server = fake_kms().await;
        mount_token(&server).await;
        let dek = crate::cipher::FieldCipher::generate_dek();
        let expected_ct = B64.encode(b"CIPHERTEXT");
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/p/locations/l/keyRings/r/cryptoKeys/hr-dek-wrap:encrypt",
            ))
            .and(header("authorization", "Bearer fake-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "ciphertext": expected_ct, "name": "…/cryptoKeyVersions/1" }),
            ))
            .mount(&server)
            .await;

        let wrapped = provider(&server)
            .wrap(
                &KekHandle {
                    id: "gcp-kms:hr-dek-wrap".into(),
                },
                &dek,
            )
            .await
            .unwrap();
        assert_eq!(wrapped.kek_id, "gcp-kms:hr-dek-wrap");
        assert_eq!(wrapped.key_id, dek.key_id);
        assert_eq!(wrapped.wrapped_key, b"CIPHERTEXT");
    }

    #[tokio::test]
    async fn unwrap_returns_32_byte_dek() {
        let server = fake_kms().await;
        mount_token(&server).await;
        let plaintext = B64.encode([7u8; 32]);
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/p/locations/l/keyRings/r/cryptoKeys/hr-dek-wrap:decrypt",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "plaintext": plaintext })),
            )
            .mount(&server)
            .await;

        let key_id = Uuid::new_v4();
        let wrapped = WrappedDek {
            key_id,
            kek_id: "gcp-kms:hr-dek-wrap".into(),
            wrapped_key: b"whatever".to_vec(),
        };
        let material = provider(&server).unwrap(&wrapped).await.unwrap();
        assert_eq!(material.key_id, key_id);
        assert_eq!(&*material.key, &[7u8; 32]);
    }

    #[tokio::test]
    async fn decrypt_4xx_maps_to_unwrap_error() {
        let server = fake_kms().await;
        mount_token(&server).await;
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/p/locations/l/keyRings/r/cryptoKeys/hr-dek-wrap:decrypt",
            ))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad ciphertext"))
            .mount(&server)
            .await;
        let wrapped = WrappedDek {
            key_id: Uuid::new_v4(),
            kek_id: "gcp-kms:hr-dek-wrap".into(),
            wrapped_key: vec![0; 8],
        };
        assert!(matches!(
            provider(&server).unwrap(&wrapped).await,
            Err(KekError::Unwrap(_))
        ));
    }

    #[tokio::test]
    async fn token_is_cached_across_calls() {
        let server = fake_kms().await;
        // Token mock: allow exactly one fetch (expect up_to_n_times enforced by count on drop).
        Mock::given(method("GET"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fake-token", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "plaintext": B64.encode([1u8; 32]) })),
            )
            .mount(&server)
            .await;
        let p = provider(&server);
        let w = WrappedDek {
            key_id: Uuid::new_v4(),
            kek_id: "gcp-kms:hr-dek-wrap".into(),
            wrapped_key: vec![0; 8],
        };
        p.unwrap(&w).await.unwrap();
        p.unwrap(&w).await.unwrap();
        // MockServer verifies on drop that the token endpoint was hit exactly once.
    }

    #[cfg(feature = "blind-index")]
    fn mac_signer(server: &MockServer) -> KmsMacSigner {
        let http = reqwest::Client::new();
        KmsMacSigner {
            resource: "projects/p/locations/l/keyRings/r/cryptoKeys/bi/cryptoKeyVersions/1"
                .to_string(),
            http: http.clone(),
            tokens: TokenSource::new(http, format!("{}/token", server.uri())),
            endpoint: server.uri(),
        }
    }

    #[cfg(feature = "blind-index")]
    #[tokio::test]
    async fn mac_sign_posts_data_and_returns_32_bytes() {
        let server = fake_kms().await;
        mount_token(&server).await;
        let expected = [3u8; 32];
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/p/locations/l/keyRings/r/cryptoKeys/bi/cryptoKeyVersions/1:macSign",
            ))
            .and(header("authorization", "Bearer fake-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "mac": B64.encode(expected) })),
            )
            .mount(&server)
            .await;
        let got = mac_signer(&server).mac_sign(b"some-data").await.unwrap();
        assert_eq!(got, expected);
    }

    #[cfg(feature = "blind-index")]
    #[tokio::test]
    async fn mac_sign_rejects_non_32_byte_mac() {
        let server = fake_kms().await;
        mount_token(&server).await;
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/p/locations/l/keyRings/r/cryptoKeys/bi/cryptoKeyVersions/1:macSign",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "mac": B64.encode([0u8; 16]) })),
            )
            .mount(&server)
            .await;
        assert!(mac_signer(&server).mac_sign(b"x").await.is_err());
    }
}
