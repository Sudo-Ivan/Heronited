//! Errors, with the words a person reads for the ones the server names.

use heronite_i18n::tr;

#[derive(Debug, Clone)]
pub enum SavesError {
    /// The server answered with a problem document.
    Api { status: u16, code: String, title: String, detail: String, retry_after: Option<u64>, head: Option<String> },
    /// The server could not be reached or the answer was cut.
    Network(String),
    /// Something on this PC (a file, the key, a refusal before sending).
    Local(String),
}

pub type Result<T> = std::result::Result<T, SavesError>;

impl SavesError {
    pub fn local(message: impl Into<String>) -> Self {
        SavesError::Local(message.into())
    }

    pub fn code(&self) -> &str {
        match self {
            SavesError::Api { code, .. } => code,
            SavesError::Network(_) => "network",
            SavesError::Local(_) => "local",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            SavesError::Api { status, .. } => *status,
            _ => 0,
        }
    }

    /// The sign-in is no longer accepted: log in again.
    pub fn is_unauthorized(&self) -> bool {
        self.status() == 401
    }

    /// Another device moved the head of the slot first.
    pub fn is_conflict(&self) -> bool {
        matches!(self, SavesError::Api { status: 409, code, title, .. } if code == "conflict" && title != "limit reached")
    }

    pub fn is_not_found(&self) -> bool {
        self.status() == 404
    }

    pub fn is_quota(&self) -> bool {
        self.code() == "quota_exceeded"
    }
}

impl std::fmt::Display for SavesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SavesError::Local(m) => f.write_str(m),
            SavesError::Network(m) => f.write_str(&tr!("Could not reach the saves server: {error}", error = m)),
            SavesError::Api { status, code, title, detail, retry_after, .. } => {
                let words: String = match code.as_str() {
                    "unauthorized" => tr!("The saves server did not accept this secure key.").into(),
                    "forbidden" => tr!("The saves server does not allow this.").into(),
                    "pending_approval" => tr!("The saves server has to approve this account first.").into(),
                    "ip_blocked" => tr!("The saves server blocks this address.").into(),
                    "quota_exceeded" => tr!("The saves server is out of space for this account.").into(),
                    "too_large" => tr!("The saves are too large for the saves server.").into(),
                    "rate_limited" | "bandwidth_exceeded" => match retry_after {
                        Some(s) => tr!("The saves server asks to wait {seconds}s and try again.", seconds = s),
                        None => tr!("The saves server asks to wait a moment and try again.").into(),
                    },
                    "maintenance" => tr!("The saves server is under maintenance. Try again later.").into(),
                    "conflict" if title != "limit reached" => tr!("Another device saved first.").into(),
                    "conflict" if title == "limit reached" => {
                        tr!("The saves server's limit of games or unfinished backups is reached.").into()
                    }
                    "encrypted" | "encryption_required" => {
                        tr!("The saves server wants encrypted saves, which Heronited does not send.").into()
                    }
                    _ => {
                        let text = if detail.is_empty() { title } else { detail };
                        return if text.is_empty() {
                            f.write_str(&tr!("The saves server answered with an error ({status}).", status = status))
                        } else {
                            f.write_str(text)
                        };
                    }
                };
                f.write_str(&words)
            }
        }
    }
}

impl std::error::Error for SavesError {}

impl From<std::io::Error> for SavesError {
    fn from(e: std::io::Error) -> Self {
        SavesError::Local(e.to_string())
    }
}
