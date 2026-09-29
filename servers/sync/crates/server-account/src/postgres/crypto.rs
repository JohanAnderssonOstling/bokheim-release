use crate::core::AuthError;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use rand::{Rng, RngCore};
use ring::{aead, hmac};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

const TOKEN_BYTES: usize = 32;
const OUTBOX_NONCE_BYTES: usize = 12;
const OUTBOX_VERSION: &str = "v1";

pub struct AccountTokenKey([u8; 32]);

impl AccountTokenKey {
    pub fn from_base64(encoded: &str) -> Result<Self, String> {
        let encoded = encoded.trim();
        let decoded = URL_SAFE_NO_PAD.decode(encoded).or_else(|_| STANDARD.decode(encoded)).map_err(|_| "account token key must be base64".to_string())?;
        let key: [u8; 32] = decoded.try_into().map_err(|_| "account token key must decode to exactly 32 bytes".to_string())?;
        Ok(Self(key))
    }

    #[cfg(test)]
    pub(super) fn test_key() -> Self {
        Self([0x5a; 32])
    }
}

pub(super) struct TokenProtector {
    mac_key: hmac::Key,
    encryption_key: aead::LessSafeKey,
}

impl TokenProtector {
    pub(super) fn new(key: AccountTokenKey) -> Self {
        let mac_key = hmac::Key::new(hmac::HMAC_SHA256, &key.0);
        let derived = hmac::sign(&mac_key, b"bokheim-account-outbox-encryption-v1");
        let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, derived.as_ref()).expect("HMAC-SHA256 always produces a valid ChaCha20-Poly1305 key");
        Self { mac_key, encryption_key: aead::LessSafeKey::new(unbound) }
    }

    pub(super) fn verification_hash(&self, email: &str, pin: &str) -> String {
        self.keyed_hash(b"bokheim-email-verification-v1", &[email.as_bytes(), pin.as_bytes()])
    }

    pub(super) fn password_reset_hash(&self, token: &str) -> String {
        self.keyed_hash(b"bokheim-password-reset-v1", &[token.as_bytes()])
    }

    fn keyed_hash(&self, domain: &[u8], values: &[&[u8]]) -> String {
        let mut context = hmac::Context::with_key(&self.mac_key);
        context.update(domain);
        for value in values {
            context.update(&[0]);
            context.update(value);
        }
        hex(context.sign().as_ref())
    }

    pub(super) fn encrypt_outbox_token(&self, id: &str, recipient: &str, kind: &str, token: &str) -> Result<String, AuthError> {
        let mut nonce_bytes = [0_u8; OUTBOX_NONCE_BYTES];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
        let mut ciphertext = token.as_bytes().to_vec();
        self.encryption_key.seal_in_place_append_tag(nonce, aead::Aad::from(outbox_aad(id, recipient, kind)), &mut ciphertext).map_err(|_| AuthError::Internal("failed to protect queued account email".to_string()))?;
        let mut encoded = nonce_bytes.to_vec();
        encoded.extend_from_slice(&ciphertext);
        Ok(format!("{OUTBOX_VERSION}.{}", URL_SAFE_NO_PAD.encode(encoded)))
    }

    pub(super) fn decrypt_outbox_token(&self, id: &str, recipient: &str, kind: &str, protected: &str) -> Result<String, AuthError> {
        let encoded = protected.strip_prefix("v1.").ok_or_else(|| AuthError::Internal("queued account email has an unsupported protection format".to_string()))?;
        let mut payload = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| AuthError::Internal("queued account email is corrupt".to_string()))?;
        if payload.len() < OUTBOX_NONCE_BYTES + aead::CHACHA20_POLY1305.tag_len() {
            return Err(AuthError::Internal("queued account email is corrupt".to_string()));
        }
        let nonce_bytes: [u8; OUTBOX_NONCE_BYTES] = payload[..OUTBOX_NONCE_BYTES].try_into().map_err(|_| AuthError::Internal("queued account email is corrupt".to_string()))?;
        let plaintext = self
            .encryption_key
            .open_in_place(aead::Nonce::assume_unique_for_key(nonce_bytes), aead::Aad::from(outbox_aad(id, recipient, kind)), &mut payload[OUTBOX_NONCE_BYTES..])
            .map_err(|_| AuthError::Internal("queued account email authentication failed".to_string()))?;
        String::from_utf8(plaintext.to_vec()).map_err(|_| AuthError::Internal("queued account email contains invalid text".to_string()))
    }
}

fn outbox_aad(id: &str, recipient: &str, kind: &str) -> Vec<u8> {
    [id.as_bytes(), &[0], recipient.as_bytes(), &[0], kind.as_bytes()].concat()
}

pub(super) fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(password.as_bytes(), &salt).map(|hash| hash.to_string()).map_err(|error| AuthError::Internal(error.to_string()))
}

pub(super) fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok()
}

/// Equalizes the missing-user and wrong-password work factors.
pub(super) fn verify_password_dummy(password: &str) {
    static DUMMY_HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let hash = DUMMY_HASH.get_or_init(|| hash_password("dummy-password-for-timing").unwrap_or_default());
    if !hash.is_empty() {
        let _ = verify_password(password, hash);
    }
}

pub(super) fn mint_token() -> (String, String) {
    mint(TOKEN_BYTES)
}

pub(super) fn mint_verification_pin(protector: &TokenProtector, email: &str) -> (String, String) {
    let pin = format!("{:06}", OsRng.gen_range(0..1_000_000_u32));
    let hash = protector.verification_hash(email, &pin);
    (pin, hash)
}

pub(super) fn mint_password_reset_token(protector: &TokenProtector) -> (String, String) {
    let (token, _) = mint(TOKEN_BYTES);
    let hash = protector.password_reset_hash(&token);
    (token, hash)
}

fn mint(bytes: usize) -> (String, String) {
    let mut buffer = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut buffer);
    let token = URL_SAFE_NO_PAD.encode(&buffer);
    let hash = sha256_hex(token.as_bytes());
    (token, hash)
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

#[cfg(test)]
mod tests {
    use super::{mint_verification_pin, AccountTokenKey, TokenProtector};

    #[test]
    fn verification_pin_contains_exactly_six_digits() {
        for _ in 0..100 {
            let protector = TokenProtector::new(AccountTokenKey::test_key());
            let (pin, hash) = mint_verification_pin(&protector, "reader@example.com");
            assert_eq!(pin.len(), 6);
            assert!(pin.bytes().all(|byte| byte.is_ascii_digit()));
            assert_eq!(hash.len(), 64);
        }
    }

    #[test]
    fn pin_hashes_are_keyed_and_scoped_to_the_email() {
        let first = TokenProtector::new(AccountTokenKey::test_key());
        let second = TokenProtector::new(AccountTokenKey([0x6b; 32]));
        assert_ne!(first.verification_hash("one@example.com", "123456"), first.verification_hash("two@example.com", "123456"));
        assert_ne!(first.verification_hash("one@example.com", "123456"), second.verification_hash("one@example.com", "123456"));
    }

    #[test]
    fn outbox_ciphertext_round_trips_and_is_bound_to_its_row() {
        let protector = TokenProtector::new(AccountTokenKey::test_key());
        let ciphertext = protector.encrypt_outbox_token("row-1", "reader@example.com", "verify_email", "012345").unwrap();
        assert!(!ciphertext.contains("012345"));
        assert_eq!(protector.decrypt_outbox_token("row-1", "reader@example.com", "verify_email", &ciphertext).unwrap(), "012345");
        assert!(protector.decrypt_outbox_token("row-2", "reader@example.com", "verify_email", &ciphertext).is_err());
        assert!(protector.decrypt_outbox_token("row-1", "other@example.com", "verify_email", &ciphertext).is_err());
    }
}
