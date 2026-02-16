/// Validate a username string.
///
/// Rules:
/// - Must be 3--24 characters long.
/// - Only ASCII alphanumeric characters and underscores allowed.
///
/// Returns `Ok(())` if valid, or `Err` with a human-readable message.
pub fn validate_username(username: &str) -> Result<(), String> {
    if username.is_empty() {
        return Err("username cannot be empty".to_string());
    }
    if username.len() < 3 {
        return Err("username must be at least 3 characters".to_string());
    }
    if username.len() > 24 {
        return Err("username must be 24 characters or less".to_string());
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err("username can only contain letters, numbers, and underscores".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_usernames() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("bob_123").is_ok());
        assert!(validate_username("ABC").is_ok());
        assert!(validate_username("a_b_c_d_e_f_g_h_i_j_k_l").is_ok()); // 23 chars
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_username("").is_err());
    }

    #[test]
    fn rejects_too_short() {
        assert!(validate_username("ab").is_err());
        assert!(validate_username("a").is_err());
    }

    #[test]
    fn rejects_too_long() {
        let long = "a".repeat(25);
        assert!(validate_username(&long).is_err());
    }

    #[test]
    fn rejects_special_characters() {
        assert!(validate_username("alice!").is_err());
        assert!(validate_username("bob@home").is_err());
        assert!(validate_username("hello world").is_err());
        assert!(validate_username("dash-name").is_err());
        assert!(validate_username("dot.name").is_err());
    }

    #[test]
    fn accepts_boundary_lengths() {
        assert!(validate_username("abc").is_ok()); // exactly 3
        assert!(validate_username(&"a".repeat(24)).is_ok()); // exactly 24
    }
}
