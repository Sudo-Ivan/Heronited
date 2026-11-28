//! What a share fails with. The texts a person reads come from [`tr!`]; the
//! variants tell the caller (and the tests) the kind of failure.

use heronite_i18n::tr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareError {
    /// The target's settings are wrong or incomplete; the message says what.
    BadConfig(String),
    /// The server could not be reached, the connection dropped or stalled.
    Network(String),
    /// 401 / bad login: the credentials are wrong.
    AuthFailed(String),
    /// 403 / refused: authenticated, but not allowed.
    Forbidden(String),
    /// The bucket, collection or remote folder does not exist.
    NotFound(String),
    /// 413 / 507 / quota: the file does not fit.
    TooLarge(String),
    /// The server's host key is not known yet; the UI offers to pin it.
    HostKeyUnknown { fingerprint: String, algorithm: String },
    /// The server's host key differs from the pinned or known one.
    HostKeyChanged { fingerprint: String, algorithm: String },
    /// A certificate problem the target's CA file or insecure flag decides.
    Tls(String),
    /// Any other answer of the server: status and a snippet of its body.
    Server { status: u16, body: String },
    /// The user cancelled through the progress callback.
    Cancelled,
    /// Reading the local file failed.
    Io(String),
}

pub type Result<T> = std::result::Result<T, ShareError>;

impl ShareError {
    pub fn config(message: impl Into<String>) -> Self {
        ShareError::BadConfig(message.into())
    }

    pub fn network(error: impl std::fmt::Display) -> Self {
        ShareError::Network(error.to_string())
    }

    pub fn io(error: impl std::fmt::Display) -> Self {
        ShareError::Io(error.to_string())
    }

    /// The words of this failure as one line for a notice or a dialog.
    pub fn text(&self) -> String {
        match self {
            ShareError::BadConfig(m) => m.clone(),
            ShareError::Network(m) => tr!("Could not reach the server: {error}", error = m),
            ShareError::AuthFailed(m) => tr!("The server refused the login: {error}", error = m),
            ShareError::Forbidden(m) => tr!("The server does not allow this: {error}", error = m),
            ShareError::NotFound(m) => tr!("Not found on the server: {error}", error = m),
            ShareError::TooLarge(m) => tr!("The file does not fit on the server: {error}", error = m),
            ShareError::HostKeyUnknown { fingerprint, .. } => tr!(
                "The server's key is not known yet ({fingerprint}). Trust it in the target's settings first.",
                fingerprint = fingerprint
            ),
            ShareError::HostKeyChanged { fingerprint, .. } => {
                tr!("The server's key changed ({fingerprint}). This can mean someone intercepts the connection.", fingerprint = fingerprint)
            }
            ShareError::Tls(m) => tr!("The server's certificate is not accepted: {error}", error = m),
            ShareError::Server { status, body } => tr!("The server answered {status}: {snippet}", status = status, snippet = body),
            ShareError::Cancelled => tr!("Cancelled").into(),
            ShareError::Io(m) => tr!("Could not read the file: {error}", error = m),
        }
    }

    /// Transient answers are retried; auth and client errors are not.
    pub(crate) fn retryable(&self) -> bool {
        match self {
            ShareError::Network(_) => true,
            ShareError::Server { status, .. } => *status >= 500 || *status == 429,
            _ => false,
        }
    }
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text())
    }
}

impl std::error::Error for ShareError {}

/// Maps an HTTP status to an error; `body` is the first bytes of the answer.
pub(crate) fn status_error(status: u16, body: &[u8]) -> ShareError {
    let snippet = String::from_utf8_lossy(body).chars().take(200).collect::<String>().trim().to_string();
    let short = |m: &str| if snippet.is_empty() { m.to_string() } else { format!("{m}: {snippet}") };
    match status {
        401 => ShareError::AuthFailed(short("401")),
        403 => ShareError::Forbidden(short("403")),
        404 => ShareError::NotFound(short("404")),
        409 => ShareError::Server { status, body: snippet },
        412 => ShareError::Server { status, body: snippet },
        413 => ShareError::TooLarge(short("413")),
        507 => ShareError::TooLarge(short("507")),
        _ if (300..400).contains(&status) => {
            ShareError::Server { status, body: format!("{} ({snippet})", tr!("the server redirects, which uploads do not follow")) }
        }
        _ => ShareError::Server { status, body: snippet },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_kinds() {
        assert!(ShareError::Network("x".into()).retryable());
        assert!(ShareError::Server { status: 500, body: String::new() }.retryable());
        assert!(ShareError::Server { status: 429, body: String::new() }.retryable());
        assert!(!ShareError::Server { status: 400, body: String::new() }.retryable());
        assert!(!ShareError::AuthFailed("x".into()).retryable());
        assert!(!ShareError::Cancelled.retryable());
    }

    #[test]
    fn statuses_map_to_kinds() {
        assert!(matches!(status_error(401, b""), ShareError::AuthFailed(_)));
        assert!(matches!(status_error(403, b""), ShareError::Forbidden(_)));
        assert!(matches!(status_error(404, b""), ShareError::NotFound(_)));
        assert!(matches!(status_error(413, b""), ShareError::TooLarge(_)));
        assert!(matches!(status_error(507, b""), ShareError::TooLarge(_)));
        assert!(matches!(status_error(302, b""), ShareError::Server { status: 302, .. }));
        assert!(matches!(status_error(500, b"boom"), ShareError::Server { status: 500, .. }));
    }
}
