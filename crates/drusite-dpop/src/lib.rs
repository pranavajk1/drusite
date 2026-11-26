// SPDX-License-Identifier: AGPL-3.0-or-later

//! DPoP (RFC 9449): proof that a request comes from the device holding a key.
//!
//! Drusite binds refresh and access tokens to a P-256 key that lives in the device's secure
//! hardware (the Secure Enclave, the Android Keystore or a TPM) where it can. The key store only
//! ever signs bytes, so this crate builds the proof around a signature it doesn't make:
//! [`signing_input`] gives the bytes to sign and [`finish`] turns the platform's signature into a
//! proof. [`verify`] runs the server's checks from RFC 9449 §4.3. Replayed `jti`s need shared
//! state, so the server tracks those itself.
//!
//! Proofs are made per connection and per token refresh, never per edit: StrongBox takes
//! 70–90 ms a signature.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::ecdsa::signature::Verifier as _;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DpopError {
    #[error("the DPoP proof is malformed: {0}")]
    Malformed(&'static str),
    #[error("the DPoP proof's key isn't a public P-256 key")]
    Key,
    #[error("the DPoP proof's signature doesn't verify")]
    Signature,
    #[error("the DPoP proof is for another request ({0})")]
    WrongRequest(&'static str),
    #[error("the DPoP proof is too old or from the future")]
    Stale,
    #[error("the DPoP proof isn't bound to this access token")]
    AccessToken,
    #[error("the signature isn't an ECDSA P-256 signature")]
    SignatureEncoding,
    #[error("the OS has no randomness")]
    Random,
}

/// A P-256 public key as a JWK (RFC 7517, RFC 7518).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jwk {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub y: String,
}

impl Jwk {
    /// From an uncompressed SEC1 point (`04 || x || y`, 65 bytes): what Apple's
    /// `SecKeyCopyExternalRepresentation` gives.
    pub fn from_sec1(point: &[u8]) -> Result<Self, DpopError> {
        if point.len() != 65 || point.first() != Some(&4) {
            return Err(DpopError::Key);
        }
        VerifyingKey::from_sec1_bytes(point).map_err(|_| DpopError::Key)?;
        Ok(Self {
            kty: "EC".to_owned(),
            crv: "P-256".to_owned(),
            x: B64.encode(&point[1..33]),
            y: B64.encode(&point[33..65]),
        })
    }

    /// From affine coordinates of any length up to 33 bytes. Java's `BigInteger.toByteArray()`
    /// can add a leading zero byte or drop leading zeros; this pads or trims to 32.
    pub fn from_xy(x: &[u8], y: &[u8]) -> Result<Self, DpopError> {
        fn fixed(v: &[u8]) -> Result<[u8; 32], DpopError> {
            let v = match v.len() {
                33 if v[0] == 0 => &v[1..],
                n if n <= 32 => v,
                _ => return Err(DpopError::Key),
            };
            let mut out = [0u8; 32];
            out[32 - v.len()..].copy_from_slice(v);
            Ok(out)
        }
        let mut point = [0u8; 65];
        point[0] = 4;
        point[1..33].copy_from_slice(&fixed(x)?);
        point[33..].copy_from_slice(&fixed(y)?);
        Self::from_sec1(&point)
    }

    fn verifying_key(&self) -> Result<VerifyingKey, DpopError> {
        if self.kty != "EC" || self.crv != "P-256" {
            return Err(DpopError::Key);
        }
        let x = B64.decode(&self.x).map_err(|_| DpopError::Key)?;
        let y = B64.decode(&self.y).map_err(|_| DpopError::Key)?;
        if x.len() != 32 || y.len() != 32 {
            return Err(DpopError::Key);
        }
        let mut point = Vec::with_capacity(65);
        point.push(4);
        point.extend(x);
        point.extend(y);
        VerifyingKey::from_sec1_bytes(&point).map_err(|_| DpopError::Key)
    }

    /// The RFC 7638 thumbprint, which a token's `cnf.jkt` binds to.
    pub fn thumbprint(&self) -> String {
        // The required members in lexicographic order, with no whitespace.
        let canonical = format!(
            r#"{{"crv":"{}","kty":"{}","x":"{}","y":"{}"}}"#,
            self.crv, self.kty, self.x, self.y
        );
        B64.encode(Sha256::digest(canonical.as_bytes()))
    }
}

/// `ath`: the hash of the access token a proof goes with.
pub fn access_token_hash(access_token: &str) -> String {
    B64.encode(Sha256::digest(access_token.as_bytes()))
}

/// What a proof covers.
#[derive(Debug, Clone, Copy)]
pub struct ProofRequest<'a> {
    /// The HTTP method, e.g. `POST`.
    pub htm: &'a str,
    /// The URL, without query or fragment.
    pub htu: &'a str,
    /// Seconds since the Unix epoch.
    pub iat: i64,
    /// The access token the proof is sent with, if any.
    pub access_token: Option<&'a str>,
    /// A nonce the server asked for, if any.
    pub nonce: Option<&'a str>,
}

/// The bytes the device key must sign: `base64url(header) "." base64url(payload)`.
pub fn signing_input(jwk: &Jwk, request: &ProofRequest<'_>) -> Result<String, DpopError> {
    let mut jti = [0u8; 16];
    getrandom::fill(&mut jti).map_err(|_| DpopError::Random)?;
    let header = json!({ "typ": "dpop+jwt", "alg": "ES256", "jwk": jwk });
    let mut payload = json!({
        "jti": B64.encode(jti),
        "htm": request.htm,
        "htu": strip(request.htu),
        "iat": request.iat,
    });
    if let Some(token) = request.access_token {
        payload["ath"] = access_token_hash(token).into();
    }
    if let Some(nonce) = request.nonce {
        payload["nonce"] = nonce.into();
    }
    Ok(format!(
        "{}.{}",
        B64.encode(header.to_string()),
        B64.encode(payload.to_string())
    ))
}

/// Finishes a proof from the key's signature over [`signing_input`]. Takes DER (what iOS's
/// `ecdsaSignatureMessageX962SHA256` and Android's `SHA256withECDSA` return) or raw `r || s`;
/// JWS needs raw `r || s` (RFC 7518 §3.4), normalised here to low-s.
pub fn finish(signing_input: &str, signature: &[u8]) -> Result<String, DpopError> {
    let signature = if signature.len() == 64 {
        Signature::from_slice(signature)
    } else {
        Signature::from_der(signature)
    }
    .map_err(|_| DpopError::SignatureEncoding)?;
    let signature = signature.normalize_s();
    Ok(format!(
        "{signing_input}.{}",
        B64.encode(signature.to_bytes())
    ))
}

/// A proof that passed [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The key's thumbprint.
    pub jkt: String,
    /// The proof's unique ID, for the server's replay check.
    pub jti: String,
    pub iat: i64,
}

/// What the server expects of a proof.
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    pub htm: &'a str,
    pub htu: &'a str,
    /// Seconds since the Unix epoch.
    pub now: i64,
    /// How far `iat` may be from `now`, in seconds.
    pub max_skew: i64,
    /// The access token sent with the proof; its hash must match `ath`.
    pub access_token: Option<&'a str>,
}

fn strip(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or_default()
}

/// The checks of RFC 9449 §4.3, except the replay check, which needs the server's state.
pub fn verify(proof: &str, expected: &Expected<'_>) -> Result<Verified, DpopError> {
    // A proof is small; refuse anything big before decoding it.
    if proof.len() > 4096 {
        return Err(DpopError::Malformed("too long"));
    }
    let mut parts = proof.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(DpopError::Malformed("not a compact JWS"));
    };
    let decode = |part: &str, what| -> Result<Value, DpopError> {
        let bytes = B64.decode(part).map_err(|_| DpopError::Malformed(what))?;
        serde_json::from_slice(&bytes).map_err(|_| DpopError::Malformed(what))
    };
    let header = decode(h, "header")?;
    let payload = decode(p, "payload")?;

    if header["typ"] != "dpop+jwt" {
        return Err(DpopError::Malformed("typ isn't dpop+jwt"));
    }
    if header["alg"] != "ES256" {
        return Err(DpopError::Malformed("alg isn't ES256"));
    }
    let jwk_value = header.get("jwk").ok_or(DpopError::Malformed("no jwk"))?;
    if jwk_value.get("d").is_some() {
        return Err(DpopError::Key);
    }
    let jwk: Jwk = serde_json::from_value(jwk_value.clone()).map_err(|_| DpopError::Key)?;
    let key = jwk.verifying_key()?;
    let signature = B64
        .decode(s)
        .ok()
        .and_then(|b| Signature::from_slice(&b).ok())
        .ok_or(DpopError::Signature)?;
    key.verify(format!("{h}.{p}").as_bytes(), &signature)
        .map_err(|_| DpopError::Signature)?;

    let text = |k: &'static str| {
        payload[k]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or(DpopError::Malformed(k))
    };
    let jti = text("jti")?;
    if jti.len() > 128 {
        return Err(DpopError::Malformed("jti"));
    }
    if text("htm")? != expected.htm {
        return Err(DpopError::WrongRequest("htm"));
    }
    if strip(text("htu")?) != strip(expected.htu) {
        return Err(DpopError::WrongRequest("htu"));
    }
    let iat = payload["iat"].as_i64().ok_or(DpopError::Malformed("iat"))?;
    if (expected.now - iat).abs() > expected.max_skew {
        return Err(DpopError::Stale);
    }
    match (expected.access_token, payload.get("ath")) {
        (Some(token), Some(ath)) if ath.as_str() == Some(&access_token_hash(token)) => {}
        (Some(_), _) => return Err(DpopError::AccessToken),
        (None, _) => {}
    }
    Ok(Verified {
        jkt: jwk.thumbprint(),
        jti: jti.to_owned(),
        iat,
    })
}

/// A key held in memory, for desktops without a usable TPM and for tests. The device's secure
/// hardware is better wherever there is one.
#[derive(Debug, Clone)]
pub struct SoftwareKey(p256::ecdsa::SigningKey);

impl SoftwareKey {
    pub fn generate() -> Result<Self, DpopError> {
        loop {
            let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
            getrandom::fill(bytes.as_mut()).map_err(|_| DpopError::Random)?;
            if let Ok(key) = p256::ecdsa::SigningKey::from_slice(bytes.as_ref()) {
                return Ok(Self(key));
            }
        }
    }

    /// The key's secret scalar, to keep it somewhere safe.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes().into()
    }

    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, DpopError> {
        p256::ecdsa::SigningKey::from_slice(bytes)
            .map(Self)
            .map_err(|_| DpopError::Key)
    }

    pub fn jwk(&self) -> Jwk {
        let point = self.0.verifying_key().to_sec1_point(false);
        #[allow(clippy::expect_used)]
        Jwk::from_sec1(point.as_bytes()).expect("a P-256 key's own point is valid")
    }

    /// An ECDSA P-256 signature over `message`, raw `r || s`: what a key store does.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        use p256::ecdsa::signature::Signer as _;
        let signature: Signature = self.0.sign(message);
        signature.to_bytes().into()
    }

    /// A finished proof for `request`.
    pub fn proof(&self, request: &ProofRequest<'_>) -> Result<String, DpopError> {
        use p256::ecdsa::signature::Signer as _;
        let input = signing_input(&self.jwk(), request)?;
        let signature: Signature = self.0.sign(input.as_bytes());
        finish(&input, signature.to_der().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const URL: &str = "https://drusite.example/oauth/token";

    fn expected(now: i64) -> Expected<'static> {
        Expected {
            htm: "POST",
            htu: URL,
            now,
            max_skew: 60,
            access_token: None,
        }
    }

    fn request(iat: i64) -> ProofRequest<'static> {
        ProofRequest {
            htm: "POST",
            htu: URL,
            iat,
            access_token: None,
            nonce: None,
        }
    }

    #[test]
    fn a_proof_verifies_and_carries_the_keys_thumbprint() {
        let key = SoftwareKey::generate().unwrap();
        let proof = key.proof(&request(1000)).unwrap();
        let ok = verify(&proof, &expected(1010)).unwrap();
        assert_eq!(ok.jkt, key.jwk().thumbprint());
        // The query string doesn't count.
        let with_query = Expected {
            htu: "https://drusite.example/oauth/token?x=1",
            ..expected(1000)
        };
        assert!(verify(&proof, &with_query).is_ok());
        // Two proofs have different jtis.
        let again = verify(&key.proof(&request(1000)).unwrap(), &expected(1000)).unwrap();
        assert_ne!(again.jti, ok.jti);
    }

    #[test]
    fn a_proof_for_anything_else_is_refused() {
        let key = SoftwareKey::generate().unwrap();
        let proof = key.proof(&request(1000)).unwrap();
        let get = Expected {
            htm: "GET",
            ..expected(1000)
        };
        assert_eq!(verify(&proof, &get), Err(DpopError::WrongRequest("htm")));
        let elsewhere = Expected {
            htu: "https://evil.example/oauth/token",
            ..expected(1000)
        };
        assert_eq!(
            verify(&proof, &elsewhere),
            Err(DpopError::WrongRequest("htu"))
        );
        assert_eq!(verify(&proof, &expected(1100)), Err(DpopError::Stale));
        assert_eq!(verify(&proof, &expected(900)), Err(DpopError::Stale));

        // Signed by another key.
        let other = SoftwareKey::generate().unwrap();
        let input = signing_input(&key.jwk(), &request(1000)).unwrap();
        let forged = {
            use p256::ecdsa::signature::Signer as _;
            let s: Signature = other.0.sign(input.as_bytes());
            finish(&input, &s.to_bytes()).unwrap()
        };
        assert_eq!(verify(&forged, &expected(1000)), Err(DpopError::Signature));
    }

    #[test]
    fn access_tokens_must_match_ath() {
        let key = SoftwareKey::generate().unwrap();
        let with = |token: Option<&'static str>| Expected {
            access_token: token,
            ..expected(1000)
        };
        let bound = key
            .proof(&ProofRequest {
                access_token: Some("token-a"),
                ..request(1000)
            })
            .unwrap();
        assert!(verify(&bound, &with(Some("token-a"))).is_ok());
        assert_eq!(
            verify(&bound, &with(Some("token-b"))),
            Err(DpopError::AccessToken)
        );
        let unbound = key.proof(&request(1000)).unwrap();
        assert_eq!(
            verify(&unbound, &with(Some("token-a"))),
            Err(DpopError::AccessToken)
        );
    }

    #[test]
    fn private_keys_and_other_algorithms_are_refused() {
        let key = SoftwareKey::generate().unwrap();
        let mut jwk = serde_json::to_value(key.jwk()).unwrap();
        jwk["d"] = "secret".into();
        let header = B64.encode(json!({"typ":"dpop+jwt","alg":"ES256","jwk":jwk}).to_string());
        let payload = B64.encode(json!({"jti":"a","htm":"POST","htu":URL,"iat":1000}).to_string());
        assert_eq!(
            verify(&format!("{header}.{payload}.AAAA"), &expected(1000)),
            Err(DpopError::Key)
        );
        let none = B64.encode(json!({"typ":"dpop+jwt","alg":"none","jwk":key.jwk()}).to_string());
        assert!(matches!(
            verify(&format!("{none}.{payload}."), &expected(1000)),
            Err(DpopError::Malformed(_))
        ));
    }

    #[test]
    fn rfc7638_thumbprint() {
        // Computed independently: sha256 of the canonical JSON, base64url.
        let jwk = Jwk {
            kty: "EC".into(),
            crv: "P-256".into(),
            x: "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU".into(),
            y: "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0".into(),
        };
        assert_eq!(
            jwk.thumbprint(),
            "oKIywvGUpTVTyxMQ3bwIIeQUudfr_CkLMjCE19ECD-U"
        );
    }

    #[test]
    fn coordinates_are_padded_or_trimmed() {
        let key = SoftwareKey::generate().unwrap();
        let jwk = key.jwk();
        let x = B64.decode(&jwk.x).unwrap();
        let y = B64.decode(&jwk.y).unwrap();
        let mut x33 = vec![0];
        x33.extend(&x);
        assert_eq!(Jwk::from_xy(&x33, &y).unwrap(), jwk);
        assert!(Jwk::from_xy(&[1; 34], &y).is_err());
    }

    #[test]
    fn software_keys_round_trip() {
        let key = SoftwareKey::generate().unwrap();
        let back = SoftwareKey::from_bytes(&key.to_bytes()).unwrap();
        assert_eq!(back.jwk(), key.jwk());
    }
}
