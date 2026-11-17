//! Web Push (D13): message encryption (RFC 8291, `aes128gcm` from RFC 8188)
//! and VAPID (RFC 8292). Pure Rust: `p256`, `hkdf`, `aes-gcm`.
//!
//! The VAPID key is derived from `token-kek` with HKDF (purpose
//! `savespace/v1/vapid`), so there is no extra secret. Rotating `token-kek`
//! changes the VAPID key; browsers then need to subscribe again.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;
use zeroize::Zeroizing;

/// The record size in the `aes128gcm` header. One record holds a whole push
/// message (at most about 4 KB).
const RECORD_SIZE: u32 = 4096;

/// Errors from encryption and signing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushError {
    /// The subscription keys are not valid.
    #[error("invalid subscription key")]
    BadKey,
    /// The message is too long for one record.
    #[error("message too long")]
    TooLong,
    /// Encryption failed.
    #[error("encryption failed")]
    Encrypt,
}

/// A browser's push subscription keys (from `PushSubscription.toJSON()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keys {
    /// The browser's P-256 public key, uncompressed (65 bytes).
    pub p256dh: Vec<u8>,
    /// The 16-byte authentication secret.
    pub auth: Vec<u8>,
}

impl Keys {
    /// Parses the base64url values a browser sends.
    ///
    /// # Errors
    ///
    /// [`PushError::BadKey`] for bad base64, a point not on the curve, or an
    /// auth secret that is not 16 bytes.
    pub fn from_base64url(p256dh: &str, auth: &str) -> Result<Self, PushError> {
        let p256dh = URL_SAFE_NO_PAD
            .decode(p256dh.trim_end_matches('='))
            .map_err(|_| PushError::BadKey)?;
        let auth = URL_SAFE_NO_PAD
            .decode(auth.trim_end_matches('='))
            .map_err(|_| PushError::BadKey)?;
        if auth.len() != 16 || p256dh.len() != 65 || PublicKey::from_sec1_bytes(&p256dh).is_err() {
            return Err(PushError::BadKey);
        }
        Ok(Self { p256dh, auth })
    }
}

fn hkdf_expand(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) -> Result<(), PushError> {
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, out)
        .map_err(|_| PushError::Encrypt)
}

/// Encrypts `plaintext` for one subscription with a given sender key and
/// salt. [`encrypt`] picks both at random; tests use fixed ones.
///
/// # Errors
///
/// [`PushError`].
pub fn encrypt_with(
    keys: &Keys,
    plaintext: &[u8],
    sender: &SecretKey,
    salt: &[u8; 16],
) -> Result<Vec<u8>, PushError> {
    // One record: the plaintext, the 0x02 delimiter, and the 16-byte tag.
    if plaintext.len() + 17 > RECORD_SIZE as usize {
        return Err(PushError::TooLong);
    }
    let ua_public = PublicKey::from_sec1_bytes(&keys.p256dh).map_err(|_| PushError::BadKey)?;
    let as_public = sender.public_key().to_sec1_point(false);
    let as_public = as_public.as_bytes();
    let shared = p256::ecdh::diffie_hellman(sender.to_nonzero_scalar(), ua_public.as_affine());

    // RFC 8291 §3.4: IKM from the ECDH secret and the auth secret.
    let mut info = Vec::with_capacity(14 + 65 + 65);
    info.extend_from_slice(b"WebPush: info\0");
    info.extend_from_slice(&keys.p256dh);
    info.extend_from_slice(as_public);
    let mut ikm = Zeroizing::new([0_u8; 32]);
    hkdf_expand(&keys.auth, shared.raw_secret_bytes(), &info, ikm.as_mut())?;

    // RFC 8188 §2.2–2.3: content key and nonce.
    let mut cek = Zeroizing::new([0_u8; 16]);
    hkdf_expand(
        salt,
        ikm.as_ref(),
        b"Content-Encoding: aes128gcm\0",
        cek.as_mut(),
    )?;
    let mut nonce = [0_u8; 12];
    hkdf_expand(salt, ikm.as_ref(), b"Content-Encoding: nonce\0", &mut nonce)?;

    let mut record = Vec::with_capacity(plaintext.len() + 1);
    record.extend_from_slice(plaintext);
    record.push(0x02);
    let cipher = Aes128Gcm::new_from_slice(cek.as_ref()).map_err(|_| PushError::Encrypt)?;
    let sealed = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &record,
                aad: &[],
            },
        )
        .map_err(|_| PushError::Encrypt)?;

    // Header: salt, record size, key ID length, key ID (the sender key).
    let mut body = Vec::with_capacity(16 + 4 + 1 + 65 + sealed.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(65);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// A random P-256 secret key.
fn random_secret() -> SecretKey {
    loop {
        let mut bytes = Zeroizing::new([0_u8; 32]);
        rand::fill(bytes.as_mut());
        if let Ok(key) = SecretKey::from_slice(bytes.as_ref()) {
            return key;
        }
    }
}

/// Encrypts a push message for one subscription (RFC 8291).
///
/// # Errors
///
/// [`PushError`].
pub fn encrypt(keys: &Keys, plaintext: &[u8]) -> Result<Vec<u8>, PushError> {
    let mut salt = [0_u8; 16];
    rand::fill(&mut salt);
    encrypt_with(keys, plaintext, &random_secret(), &salt)
}

/// The application server key for VAPID (RFC 8292).
#[derive(Clone)]
pub struct Vapid {
    signing: SigningKey,
    /// The public key, uncompressed, base64url. Browsers pass it to
    /// `pushManager.subscribe({ applicationServerKey })`.
    pub public_key: String,
    /// The `sub` claim: a contact URL. The app origin, not a person's email.
    pub subject: String,
}

impl std::fmt::Debug for Vapid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vapid")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

impl Vapid {
    /// Derives the VAPID key from key material (`token-kek` bytes) with HKDF.
    /// The same input always gives the same key.
    pub fn derive(master: &[u8], subject: &str) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(b"savespace/v1/vapid"), master);
        let mut counter: u8 = 0;
        let secret = loop {
            let mut bytes = Zeroizing::new([0_u8; 32]);
            let info = [b"p256 key ".as_slice(), &[counter]].concat();
            // 32 bytes is a valid HKDF-SHA256 output length.
            if hk.expand(&info, bytes.as_mut()).is_ok()
                && let Ok(key) = SecretKey::from_slice(bytes.as_ref())
            {
                break key;
            }
            counter = counter.wrapping_add(1);
        };
        let public = secret.public_key().to_sec1_point(false);
        Self {
            signing: SigningKey::from(&secret),
            public_key: URL_SAFE_NO_PAD.encode(public.as_bytes()),
            subject: subject.to_owned(),
        }
    }

    /// The `Authorization` header value for a push to `endpoint`:
    /// `vapid t=<JWT>, k=<public key>`. The token is valid for 12 hours.
    pub fn authorization(&self, endpoint: &url::Url, now: jiff::Timestamp) -> String {
        let audience = endpoint.origin().ascii_serialization();
        let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = serde_json::json!({
            "aud": audience,
            "exp": now.as_second() + 12 * 3600,
            "sub": self.subject,
        });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signing_input = format!("{header}.{claims}");
        let signature: Signature = self.signing.sign(signing_input.as_bytes());
        let jwt = format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        );
        format!("vapid t={jwt}, k={}", self.public_key)
    }
}

/// Push service hosts SaveSpace sends to. A subscription endpoint must be on
/// one of them, so a client cannot make the server call any URL (SSRF).
pub const PUSH_HOSTS: &[&str] = &[
    "fcm.googleapis.com",
    "updates.push.services.mozilla.com",
    "web.push.apple.com",
    "wns2-par02p.notify.windows.com",
];

/// True for an `https` endpoint on a known push service.
pub fn allowed_endpoint(endpoint: &url::Url) -> bool {
    endpoint.scheme() == "https"
        && endpoint.port().is_none()
        && endpoint.host_str().is_some_and(|h| {
            PUSH_HOSTS.contains(&h)
                || h.ends_with(".notify.windows.com")
                || h.ends_with(".push.apple.com")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier;

    fn b64(s: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD.decode(s).unwrap()
    }

    /// RFC 8291 Section 5 / Appendix A.
    #[test]
    fn matches_the_rfc_8291_example() {
        let keys = Keys {
            p256dh: b64(
                "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
            ),
            auth: b64("BTBZMqHH6r4Tts7J_aSIgg"),
        };
        let sender =
            SecretKey::from_slice(&b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = encrypt_with(
            &keys,
            b"When I grow up, I want to be a watermelon",
            &sender,
            &salt,
        )
        .unwrap();
        assert_eq!(
            URL_SAFE_NO_PAD.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    /// Decrypting with the browser's private key gives the message back.
    #[test]
    fn round_trips_with_the_browser_key() {
        let ua = random_secret();
        let mut auth = [0_u8; 16];
        rand::fill(&mut auth);
        let keys = Keys {
            p256dh: ua.public_key().to_sec1_point(false).as_bytes().to_vec(),
            auth: auth.to_vec(),
        };
        let body = encrypt(&keys, b"Rent is expected on Dec 1.").unwrap();
        let (salt, rest) = body.split_at(16);
        let as_public = PublicKey::from_sec1_bytes(&rest[5..70]).unwrap();
        let shared = p256::ecdh::diffie_hellman(ua.to_nonzero_scalar(), as_public.as_affine());
        let info = [b"WebPush: info\0".as_slice(), &keys.p256dh, &rest[5..70]].concat();
        let mut ikm = [0_u8; 32];
        hkdf_expand(&auth, shared.raw_secret_bytes(), &info, &mut ikm).unwrap();
        let (mut cek, mut nonce) = ([0_u8; 16], [0_u8; 12]);
        hkdf_expand(salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek).unwrap();
        hkdf_expand(salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce).unwrap();
        let plain = Aes128Gcm::new_from_slice(&cek)
            .unwrap()
            .decrypt(&Nonce::from(nonce), &rest[70..])
            .unwrap();
        assert_eq!(plain, b"Rent is expected on Dec 1.\x02");
    }

    #[test]
    fn vapid_tokens_verify_with_the_public_key() {
        let vapid = Vapid::derive(&[7_u8; 32], "https://app.example.test");
        assert_eq!(
            vapid.public_key,
            Vapid::derive(&[7_u8; 32], "x").public_key,
            "deterministic"
        );
        assert_ne!(vapid.public_key, Vapid::derive(&[8_u8; 32], "x").public_key);
        let endpoint: url::Url = "https://fcm.googleapis.com/fcm/send/abc".parse().unwrap();
        let header = vapid.authorization(
            &endpoint,
            jiff::Timestamp::from_second(1_800_000_000).unwrap(),
        );
        let (jwt, key) = header
            .strip_prefix("vapid t=")
            .unwrap()
            .split_once(", k=")
            .unwrap();
        assert_eq!(key, vapid.public_key);
        let (input, sig) = jwt.rsplit_once('.').unwrap();
        let verifying = VerifyingKey::from_sec1_bytes(&b64(key)).unwrap();
        let signature = Signature::from_slice(&b64(sig)).unwrap();
        verifying.verify(input.as_bytes(), &signature).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&b64(input.split('.').nth(1).unwrap())).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["exp"], 1_800_000_000 + 43_200);
    }

    #[test]
    fn only_known_push_services() {
        let ok = |s: &str| allowed_endpoint(&s.parse().unwrap());
        assert!(ok("https://fcm.googleapis.com/fcm/send/abc"));
        assert!(ok("https://updates.push.services.mozilla.com/wpush/v2/x"));
        assert!(ok("https://web.push.apple.com/QG"));
        assert!(!ok("http://fcm.googleapis.com/x"));
        assert!(!ok("https://fcm.googleapis.com:8443/x"));
        assert!(!ok("https://169.254.169.254/computeMetadata"));
        assert!(!ok("https://evil.example/fcm.googleapis.com"));
        assert!(!ok("https://fcm.googleapis.com.evil.example/x"));
    }

    #[test]
    fn rejects_bad_subscription_keys() {
        assert_eq!(Keys::from_base64url("abc", "def"), Err(PushError::BadKey));
        let good = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        assert!(Keys::from_base64url(good, "BTBZMqHH6r4Tts7J_aSIgg").is_ok());
        assert_eq!(Keys::from_base64url(good, "AAAA"), Err(PushError::BadKey));
    }
}
