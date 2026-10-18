//! Random secrets, their hashes, and PKCE.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// A new random secret with a readable prefix, e.g. `frood_at_…`. 256 bits of entropy.
pub(crate) fn new_secret(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system provides randomness");
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Secrets are stored only as hashes. They are high-entropy, so a fast hash is enough.
pub(crate) fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// RFC 7636 S256: `BASE64URL(SHA256(verifier)) == challenge`.
pub(crate) fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let valid_verifier = (43..=128).contains(&verifier.len())
        && verifier.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b));
    valid_verifier && URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge
}

pub(crate) fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_prefixed_and_unique() {
        let a = new_secret("frood_at_");
        let b = new_secret("frood_at_");
        assert!(a.starts_with("frood_at_") && a.len() > 40);
        assert_ne!(a, b);
        assert_eq!(hash(&a).len(), 64);
    }

    #[test]
    fn pkce_follows_rfc_7636_example() {
        // Appendix B of RFC 7636.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert!(pkce_matches(verifier, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!pkce_matches(verifier, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cX"));
        assert!(!pkce_matches("short", &pkce_challenge("short")));
    }
}
