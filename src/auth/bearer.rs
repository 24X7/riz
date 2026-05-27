use subtle::ConstantTimeEq;

/// Constant-time bearer token validator.
/// Returns true iff the Authorization header matches `Bearer <expected>` exactly.
pub fn validate_bearer(authorization_header: Option<&str>, expected_token: &str) -> bool {
    let Some(h) = authorization_header else {
        return false;
    };
    let trimmed = h.trim();
    let Some(token) = trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
    else {
        return false;
    };
    let token = token.trim();
    token.as_bytes().ct_eq(expected_token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_correct_bearer_token() {
        assert!(validate_bearer(Some("Bearer secret123"), "secret123"));
    }

    #[test]
    fn rejects_missing_authorization() {
        assert!(!validate_bearer(None, "secret123"));
    }

    #[test]
    fn rejects_wrong_token() {
        assert!(!validate_bearer(Some("Bearer wrong"), "secret123"));
    }

    #[test]
    fn rejects_wrong_scheme() {
        assert!(!validate_bearer(Some("Basic c2VjcmV0"), "secret123"));
    }

    #[test]
    fn accepts_lowercase_bearer_scheme() {
        assert!(validate_bearer(Some("bearer secret123"), "secret123"));
    }

    #[test]
    fn rejects_token_length_mismatch() {
        assert!(!validate_bearer(Some("Bearer short"), "much-longer-token"));
    }
}
