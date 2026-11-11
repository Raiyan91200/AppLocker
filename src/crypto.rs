//! Master password hashing with Argon2id.
//!
//! A hash is one-way: the password is never stored or decrypted, only verified.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// Returns a PHC string like `$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`.
pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing failed")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify() {
        let h = hash_password("hunter2");
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("hunter2", &h));
        assert!(!verify_password("hunter3", &h));
        assert!(!verify_password("", &h));
    }

    #[test]
    fn same_password_different_salt() {
        assert_ne!(hash_password("abc"), hash_password("abc"));
    }

    #[test]
    fn garbage_hash_is_rejected() {
        assert!(!verify_password("x", "not a hash"));
    }
}
