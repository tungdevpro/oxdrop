use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use subtle::ConstantTimeEq;

/// The unguessable part of a Transfer Link: 128 random bits, base64url-encoded.
#[derive(Clone, PartialEq, Eq)]
pub struct TransferSecret(String);

impl TransferSecret {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).expect("the OS random number generator is available");
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Whether `candidate` is this secret, compared in constant time.
    pub fn matches(&self, candidate: &str) -> bool {
        self.0.as_bytes().ct_eq(candidate.as_bytes()).into()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TransferSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Never print the secret in debug output (logs, panics).
impl fmt::Debug for TransferSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TransferSecret(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secrets_are_22_url_safe_chars_and_distinct() {
        let a = TransferSecret::generate();
        let b = TransferSecret::generate();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 22);
        assert!(
            a.as_str()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn matches_only_the_exact_secret() {
        let s = TransferSecret::generate();
        assert!(s.matches(s.as_str()));
        assert!(!s.matches(&s.as_str()[1..]));
        assert!(!s.matches(""));
        assert!(!s.matches(&TransferSecret::generate().to_string()));
    }

    #[test]
    fn debug_output_hides_the_secret() {
        let s = TransferSecret::generate();
        assert!(!format!("{s:?}").contains(s.as_str()));
    }
}
