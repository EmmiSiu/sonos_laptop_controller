//! The shared error vocabulary, and the redaction applied before an error reaches a human.
//!
//! Two rules govern errors in Rincon:
//!
//! 1. **Every error is typed.** A `String` error is a decision not to handle the failure, and
//!    it is exactly the thing that produces "Something went wrong" in a shipped app.
//! 2. **Every user-visible error is redacted.** A raw error may legitimately carry an absolute
//!    path or a full LAN address for the log file; neither belongs in a dialog the user may
//!    screenshot into a public issue.

use std::fmt;

/// Convenience alias for fallible core operations.
pub type Result<T, E = CoreError> = std::result::Result<T, E>;

/// Failures produced by the domain layer itself.
///
/// Networking, audio, and protocol crates define their own error types and convert into this
/// only where they cross back into shared vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    /// A destination failed the private-address guard.
    #[error(transparent)]
    Net(#[from] crate::net::NetError),

    /// An XML document was refused before parsing.
    #[error(transparent)]
    Xml(#[from] crate::xml::XmlGuardError),

    /// A domain value failed its construction invariant.
    #[error("invalid {field}: {reason}")]
    Invalid {
        /// Which field failed.
        field: &'static str,
        /// Why, in terms a maintainer can act on.
        reason: String,
    },

    /// A read exceeded its configured budget.
    #[error("{what} exceeded its limit of {limit} {unit}")]
    LimitExceeded {
        /// The operation that overran.
        what: &'static str,
        /// The configured ceiling.
        limit: u64,
        /// Unit for `limit`, e.g. `"bytes"` or `"ms"`.
        unit: &'static str,
    },
}

impl CoreError {
    /// Builds an [`CoreError::Invalid`] without the caller writing the struct literal.
    pub fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid { field, reason: reason.into() }
    }
}

/// Anything that can present itself safely to an end user.
///
/// Implementors return a sentence a non-technical person can act on. The blanket redaction in
/// [`redact_sensitive`] is applied by [`UserFacing::user_message`], so an implementor cannot
/// forget it.
pub trait UserFacing: fmt::Display {
    /// A short, actionable sentence. Defaults to the redacted `Display` output.
    fn user_message(&self) -> String {
        redact_sensitive(&self.to_string())
    }

    /// Whether retrying the same action could plausibly succeed.
    fn retryable(&self) -> bool {
        false
    }
}

impl UserFacing for CoreError {
    fn retryable(&self) -> bool {
        matches!(self, Self::LimitExceeded { .. })
    }
}

/// Removes absolute filesystem paths and full IP addresses from a message.
///
/// Redaction is applied to the *message*, not at the display site, so the guarantee holds no
/// matter which surface renders the error.
///
/// Covers: RQ-SEC-014
#[must_use]
pub fn redact_sensitive(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;

    while !rest.is_empty() {
        if let Some(consumed) = take_windows_path(rest).or_else(|| take_unix_path(rest)) {
            out.push_str("<path>");
            rest = &rest[consumed..];
        } else if let Some((consumed, redacted)) = take_ipv4(rest) {
            out.push_str(&redacted);
            rest = &rest[consumed..];
        } else {
            let ch_len = rest.chars().next().map_or(1, char::len_utf8);
            out.push_str(&rest[..ch_len]);
            rest = &rest[ch_len..];
        }
    }
    out
}

/// Matches `C:\...` or `C:/...` up to the next whitespace or quote.
fn take_windows_path(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let drive = *bytes.first()?;
    if !drive.is_ascii_alphabetic() || bytes.get(1) != Some(&b':') {
        return None;
    }
    let sep = *bytes.get(2)?;
    if sep != b'\\' && sep != b'/' {
        return None;
    }
    Some(path_run_len(s, 3))
}

/// Matches an absolute POSIX path with at least two components, so a bare `/` is left alone.
fn take_unix_path(s: &str) -> Option<usize> {
    if !s.starts_with('/') {
        return None;
    }
    let len = path_run_len(s, 1);
    let body = s.get(1..len)?;
    if body.contains('/') && body.len() > 2 { Some(len) } else { None }
}

fn path_run_len(s: &str, from: usize) -> usize {
    s.char_indices()
        .skip_while(|(i, _)| *i < from)
        .find(|(_, c)| c.is_whitespace() || *c == '"' || *c == '\'' || *c == ')')
        .map_or(s.len(), |(i, _)| i)
}

/// Replaces the final octet of a dotted-quad with `xxx`, keeping the subnet for debugging.
fn take_ipv4(s: &str) -> Option<(usize, String)> {
    let end = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit() && *c != '.')
        .map_or(s.len(), |(i, _)| i);
    let candidate = s.get(..end)?;

    // Destructured rather than indexed: exactly four octets and nothing after them.
    let mut octets = candidate.split('.');
    let (Some(net), Some(sub), Some(host), Some(last), None) =
        (octets.next(), octets.next(), octets.next(), octets.next(), octets.next())
    else {
        return None;
    };
    if [net, sub, host, last].iter().any(|part| part.parse::<u8>().is_err()) {
        return None;
    }
    Some((end, format!("{net}.{sub}.{host}.xxx")))
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

    use super::*;

    /// Covers: RQ-SEC-014
    #[test]
    fn rq_sec_014_errors_do_not_leak_paths() {
        let cases = [
            (
                r"failed to open C:\Users\alice\Documents\secret\config.toml for writing",
                "failed to open <path> for writing",
            ),
            ("could not read /home/alice/.ssh/id_rsa now", "could not read <path> now"),
            ("device 192.168.1.45 unreachable", "device 192.168.1.xxx unreachable"),
            (r"C:/Users/bob/app.log and 10.0.0.7 both failed", "<path> and 10.0.0.xxx both failed"),
        ];
        for (raw, expected) in cases {
            assert_eq!(redact_sensitive(raw), expected, "input: {raw}");
        }
    }

    #[test]
    fn redaction_leaves_ordinary_prose_alone() {
        let msg = "Kitchen is playing something else. Take over?";
        assert_eq!(redact_sensitive(msg), msg);
        // Version numbers must survive: they are four-part but not octets.
        assert_eq!(redact_sensitive("firmware 15.2.3.400"), "firmware 15.2.3.400");
    }

    #[test]
    fn user_facing_applies_redaction_automatically() {
        let err = CoreError::invalid("location", r"host C:\weird\path rejected");
        assert!(!err.user_message().contains("weird"), "message: {}", err.user_message());
    }

    #[test]
    fn limit_errors_are_retryable_and_others_are_not() {
        let limit = CoreError::LimitExceeded { what: "description", limit: 262_144, unit: "bytes" };
        assert!(limit.retryable());
        assert!(!CoreError::invalid("id", "empty").retryable());
    }

    #[test]
    fn redaction_never_panics_on_multibyte_input() {
        for raw in ["café /var/log/naïve.log 192.168.0.1", "日本語 C:\\テスト\\x", "…", ""]
        {
            let _ = redact_sensitive(raw);
        }
    }
}
