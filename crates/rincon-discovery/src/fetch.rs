//! Bounded HTTP GET: the only way this workspace fetches a document from a device.
//!
//! Three properties make this module worth existing instead of calling `reqwest` inline:
//!
//! 1. **Redirects are off.** The private-address guard runs against the URL we were given. If
//!    the client then followed a `302` to a public host, the guard would have validated an
//!    address nobody ever connected to — SSRF, reopened.
//! 2. **The body is capped while it streams.** `Content-Length` is attacker-controlled, so
//!    trusting it is not a cap. Bytes are counted as they arrive and the transfer is abandoned
//!    the moment the budget is exceeded.
//! 3. **Every call carries a deadline** from [`rincon_core::limits::Budget`], so a device that
//!    accepts a connection and then goes silent cannot stall a scan.

use bytes::Bytes;
use rincon_core::limits::Budget;
use url::Url;

/// Why a fetch did not produce a usable document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The client could not be constructed.
    #[error("HTTP client unavailable: {0}")]
    ClientUnavailable(String),

    /// The connection failed or timed out.
    #[error("request failed: {0}")]
    Transport(String),

    /// The device answered with a non-success status.
    #[error("device answered with HTTP {0}")]
    Status(u16),

    /// The body exceeded the budget and the transfer was abandoned.
    #[error("{what} exceeded its {limit} byte budget")]
    TooLarge {
        /// Which budget was overrun.
        what: &'static str,
        /// The ceiling.
        limit: usize,
    },

    /// The body was not valid UTF-8 text.
    #[error("response body is not valid UTF-8")]
    NotUtf8,
}

/// Builds the shared HTTP client.
///
/// Note the absence of any TLS feature: Sonos local control is plain HTTP, so the client is
/// built without a TLS stack at all. That removes an entire dependency subtree — and with it
/// the OpenSSL that `deny.toml` bans — rather than carrying it unused.
///
/// # Errors
///
/// Returns [`FetchError::ClientUnavailable`] when the client cannot be constructed.
pub fn client() -> Result<reqwest::Client, FetchError> {
    reqwest::Client::builder()
        // Covers: RQ-DISC-005
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Budget::DESCRIPTION.timeout)
        .connect_timeout(Budget::DESCRIPTION.timeout)
        .user_agent(concat!("Rincon/", env!("CARGO_PKG_VERSION")))
        // No connection reuse across scans: a speaker that changed address between scans
        // should not inherit a pooled socket to the old one.
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|source| FetchError::ClientUnavailable(source.to_string()))
}

/// Performs a GET, enforcing `budget` on both time and bytes.
///
/// # Errors
///
/// Returns [`FetchError`] describing the first rule the exchange violated.
///
/// Covers: RQ-DISC-005, RQ-DISC-006, RQ-SEC-003
pub async fn get_bounded(
    client: &reqwest::Client,
    url: Url,
    budget: Budget,
) -> Result<String, FetchError> {
    let bytes = get_bounded_bytes(client, url, budget).await?;
    String::from_utf8(bytes.to_vec()).map_err(|_| FetchError::NotUtf8)
}

/// [`get_bounded`] without the UTF-8 requirement.
///
/// # Errors
///
/// Returns [`FetchError`] describing the first rule the exchange violated.
pub async fn get_bounded_bytes(
    client: &reqwest::Client,
    url: Url,
    budget: Budget,
) -> Result<Bytes, FetchError> {
    let response = tokio::time::timeout(budget.timeout, client.get(url).send())
        .await
        .map_err(|_| FetchError::Transport(format!("timed out after {:?}", budget.timeout)))?
        .map_err(|source| FetchError::Transport(source.to_string()))?;

    if !response.status().is_success() {
        return Err(FetchError::Status(response.status().as_u16()));
    }

    // A declared Content-Length over budget lets us refuse before reading a single byte. It is
    // an optimisation, never the enforcement: the streaming check below is what actually holds.
    if let Some(declared) = response.content_length() {
        if declared > budget.max_bytes as u64 {
            return Err(FetchError::TooLarge { what: budget.what, limit: budget.max_bytes });
        }
    }

    read_capped(response, budget).await
}

async fn read_capped(mut response: reqwest::Response, budget: Budget) -> Result<Bytes, FetchError> {
    let mut collected = Vec::with_capacity(8 * 1024);
    loop {
        let next = tokio::time::timeout(budget.timeout, response.chunk())
            .await
            .map_err(|_| FetchError::Transport("body read timed out".to_owned()))?
            .map_err(|source| FetchError::Transport(source.to_string()))?;

        let Some(chunk) = next else { break };
        if collected.len() + chunk.len() > budget.max_bytes {
            // Dropping the response aborts the transfer; we never buffer past the ceiling.
            return Err(FetchError::TooLarge { what: budget.what, limit: budget.max_bytes });
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(collected))
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::float_cmp,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use rincon_core::limits;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn a_normal_document_is_fetched_intact() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xml/device_description.xml"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<root>ok</root>"))
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/xml/device_description.xml", server.uri())).unwrap();
        let body = get_bounded(&client().unwrap(), url, Budget::DESCRIPTION).await.unwrap();
        assert_eq!(body, "<root>ok</root>");
    }

    /// Covers: RQ-DISC-005
    #[tokio::test]
    async fn rq_disc_005_does_not_follow_redirects() {
        // The attack: a device answers the validated private URL with a redirect to a public
        // host. Following it would mean the guard validated an address we never connected to.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "http://93.184.216.34/evil"),
            )
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/redirect", server.uri())).unwrap();
        let result = get_bounded(&client().unwrap(), url, Budget::DESCRIPTION).await;

        match result {
            Err(FetchError::Status(302)) => {}
            other => panic!("a redirect must surface as a status, not be followed: {other:?}"),
        }
    }

    /// Covers: RQ-DISC-006
    #[tokio::test]
    async fn rq_disc_006_caps_description_size() {
        let server = MockServer::start().await;
        let oversized = "x".repeat(limits::MAX_XML_BYTES + 4096);
        Mock::given(method("GET"))
            .and(path("/huge"))
            .respond_with(ResponseTemplate::new(200).set_body_string(oversized))
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/huge", server.uri())).unwrap();
        let result = get_bounded(&client().unwrap(), url, Budget::DESCRIPTION).await;

        assert!(
            matches!(result, Err(FetchError::TooLarge { .. })),
            "an oversized body must be refused, got {result:?}"
        );
    }

    #[tokio::test]
    async fn a_body_exactly_at_the_budget_is_accepted() {
        // Off-by-one here would reject legitimate large descriptions on a big household.
        let server = MockServer::start().await;
        let budget = Budget { what: "test", max_bytes: 1024, timeout: Budget::DESCRIPTION.timeout };
        Mock::given(method("GET"))
            .and(path("/exact"))
            .respond_with(ResponseTemplate::new(200).set_body_string("y".repeat(1024)))
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/exact", server.uri())).unwrap();
        let body = get_bounded(&client().unwrap(), url, budget).await.unwrap();
        assert_eq!(body.len(), 1024);
    }

    #[tokio::test]
    async fn an_error_status_is_typed_rather_than_parsed_as_a_document() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/missing", server.uri())).unwrap();
        let result = get_bounded(&client().unwrap(), url, Budget::DESCRIPTION).await;
        assert_eq!(result.unwrap_err(), FetchError::Status(404));
    }

    /// Covers: RQ-SEC-003
    #[tokio::test]
    async fn rq_sec_003_a_stalled_device_cannot_hang_a_scan() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<root/>")
                    .set_delay(std::time::Duration::from_secs(30)),
            )
            .mount(&server)
            .await;

        let budget = Budget {
            what: "test",
            max_bytes: 4096,
            timeout: std::time::Duration::from_millis(250),
        };
        let url = Url::parse(&format!("{}/slow", server.uri())).unwrap();

        let started = std::time::Instant::now();
        let result = get_bounded(&client().unwrap(), url, budget).await;
        let elapsed = started.elapsed();

        assert!(result.is_err(), "a stalled device must not produce a document");
        assert!(elapsed < std::time::Duration::from_secs(5), "gave up only after {elapsed:?}");
    }

    #[tokio::test]
    async fn a_non_utf8_body_is_refused_rather_than_lossily_decoded() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/binary"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0xC3, 0x28, 0xFF]))
            .mount(&server)
            .await;

        let url = Url::parse(&format!("{}/binary", server.uri())).unwrap();
        let result = get_bounded(&client().unwrap(), url, Budget::DESCRIPTION).await;
        assert_eq!(result.unwrap_err(), FetchError::NotUtf8);
    }
}
