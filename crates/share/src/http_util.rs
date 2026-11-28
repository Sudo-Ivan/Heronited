//! The ureq agent and the retry loop the HTTP-ish backends share.

use std::time::Duration;

use crate::error::{status_error, Result, ShareError};
use crate::tls::TlsOptions;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub const STALL_TIMEOUT: Duration = Duration::from_secs(60);

use std::sync::atomic::{AtomicU64, Ordering};

#[doc(hidden)]
pub static CONNECT_TIMEOUT_MS: AtomicU64 = AtomicU64::new(15_000);
#[doc(hidden)]
pub static STALL_TIMEOUT_MS: AtomicU64 = AtomicU64::new(60_000);

/// Tests shorten the timeouts; production never calls this.
#[doc(hidden)]
pub fn set_timeouts(connect: Duration, stall: Duration) {
    CONNECT_TIMEOUT_MS.store(connect.as_millis() as u64, Ordering::Relaxed);
    STALL_TIMEOUT_MS.store(stall.as_millis() as u64, Ordering::Relaxed);
}

pub fn agent(tls: &TlsOptions) -> Result<ureq::Agent> {
    let tls_config = tls.tls_config()?;
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_millis(CONNECT_TIMEOUT_MS.load(Ordering::Relaxed))))
        .timeout_global(None)
        .timeout_recv_response(Some(Duration::from_millis(STALL_TIMEOUT_MS.load(Ordering::Relaxed))))
        .timeout_recv_body(Some(Duration::from_millis(STALL_TIMEOUT_MS.load(Ordering::Relaxed))))
        .tls_config(tls_config)
        .user_agent(concat!("Heronited/", env!("CARGO_PKG_VERSION")))
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        // Redirects off: credentials must never follow one to another host.
        .max_redirects(0)
        .build();
    Ok(ureq::Agent::new_with_config(config))
}

/// Classifies a ureq send error. TLS problems get their own kind.
pub fn send_error(err: ureq::Error) -> ShareError {
    let text = err.to_string();
    let lower = text.to_lowercase();
    if lower.contains("certificate") || lower.contains("tls") || lower.contains("x509") || lower.contains("ssl") {
        return ShareError::Tls(text);
    }
    ShareError::Network(text)
}

/// Reads the first bytes of a body for an error message.
pub fn snippet(response: &mut ureq::http::Response<ureq::Body>) -> Vec<u8> {
    use std::io::Read as _;
    let mut buf = vec![0u8; 512];
    let n = response.body_mut().as_reader().read(&mut buf).unwrap_or(0);
    buf.truncate(n);
    buf
}

/// Runs `op` up to three times on transient failures, backing off a little.
pub fn retried<T>(mut op: impl FnMut() -> Result<T>) -> Result<T> {
    let mut wait = Duration::from_millis(150);
    for attempt in 0..3 {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if e.retryable() && attempt < 2 => {
                log::debug!("share: transient failure, retrying: {e}");
                std::thread::sleep(wait);
                wait *= 3;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// A 2xx is fine; anything else is a typed error with the body's first bytes.
pub fn expect_ok(response: &mut ureq::http::Response<ureq::Body>, ok: impl Fn(u16) -> bool) -> Result<()> {
    let status = response.status().as_u16();
    if ok(status) {
        return Ok(());
    }
    let body = snippet(response);
    Err(status_error(status, &body))
}
