//! HTTP calls to `nirdosha serve`'s presence bridge
//! (`POST /api/_presence_connect`/`_disconnect`, `docs/WORKFLOW.md`'s "notify
//! presence bridge" section) — the machine-to-machine side of the
//! contract this gateway is the *other* end of. Authenticated with a
//! service credential (`--presence-token`), the same one `nirdosha serve`
//! was started with, constant-time-compared on its side
//! (`serve.rs::handle_presence`) — not a normal per-user identity token.

use std::time::Duration;

#[derive(Clone)]
pub struct PresenceClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

impl PresenceClient {
    pub fn new(base_url: String, token: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest::Client::builder with only a timeout set never fails to build");
        Self { http, base_url, token }
    }

    async fn set(&self, subject: &str, online: bool) -> Result<(), String> {
        let route = if online { "_presence_connect" } else { "_presence_disconnect" };
        let url = format!("{}/api/{route}", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "subject": subject }))
            .send()
            .await
            .map_err(|e| format!("{route} request failed: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(format!("{route} returned {status}: {body}"))
        }
    }

    /// A bounded number of retries with a short fixed backoff —
    /// `_presence_connect`/`_disconnect` are two ordinary HTTP calls to
    /// `nirdosha serve`, which can be transiently unreachable for reasons
    /// that have nothing to do with any one request (a rolling deploy, a
    /// brief network blip): worth a few retries before giving up, same
    /// "don't fail on the first transient blip" posture `docs/TRANSACT.md`'s
    /// own `network` retry budget takes — deliberately much smaller here,
    /// since this is best-effort presence bookkeeping, not a durable
    /// transaction log entry.
    async fn set_with_retry(&self, subject: &str, online: bool) -> Result<(), String> {
        const ATTEMPTS: u32 = 3;
        let mut last_err = String::new();
        for attempt in 0..ATTEMPTS {
            match self.set(subject, online).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = e;
                    if attempt + 1 < ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(200 * u64::from(attempt + 1))).await;
                    }
                }
            }
        }
        Err(last_err)
    }

    pub async fn connect(&self, subject: &str) -> Result<(), String> {
        self.set_with_retry(subject, true).await
    }

    pub async fn disconnect(&self, subject: &str) -> Result<(), String> {
        self.set_with_retry(subject, false).await
    }
}
