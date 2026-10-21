//! The adoption proof (design D24): how a candidate proves it is the unit whose
//! sticker password the admin typed at the main router, and how the main router
//! proves it holds that password too, without the password crossing the
//! backhaul.
//!
//! Transport-agnostic. The main router runs an [`Adopter`] per candidate; the
//! candidate runs a [`Responder`]. Three messages:
//!
//! 1. Main router → candidate: [`Hello`], the main router's WireGuard public
//!    key and a fresh challenge.
//! 2. Candidate → main router: [`Offer`], its own public key, its own challenge
//!    and its proof.
//! 3. Main router → candidate: [`Confirm`], the tunnel the access point is to
//!    bring up and the main router's proof over it, or [`Reject`].
//!
//! Each proof is a keyed BLAKE3 hash over both challenges and both public keys,
//! keyed by the adoption secret, with a direction label so the two proofs can
//! never stand in for each other; the main router's also covers the tunnel it
//! dictates. A replayed proof fails on the fresh challenge and a substituted key
//! fails on the transcript. The tunnel's pre-shared key is derived from the same
//! material on both sides and never sent. Both sides refuse after
//! [`ATTEMPT_LIMIT`] bad proofs for [`COOLDOWN`].

use std::time::{Duration, Instant};

use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::wg::WgPublicKey;

/// Bad proofs tolerated before a cooldown starts.
pub const ATTEMPT_LIMIT: u8 = 5;
/// How long a side refuses proofs after `ATTEMPT_LIMIT` failures.
pub const COOLDOWN: Duration = Duration::from_secs(60);
/// Generated adoption code length for a board without an EEPROM password.
/// Alphanumeric from `PASSWORD_CHARS_ALNUM`: 16 characters is about 93 bits.
pub const CODE_LEN: usize = 16;

const KEY_CONTEXT: &str = "StartWRT access point adoption v1 proof";
const PSK_CONTEXT: &str = "StartWRT access point adoption v1 preshared key";
const LABEL_ACCESS_POINT: &[u8] = b"access-point->main";
const LABEL_MAIN: &[u8] = b"main->access-point";

/// A code for the pre-adoption page of a board with no sticker password.
pub fn generate_code() -> String {
    crate::generate_password(crate::PASSWORD_CHARS_ALNUM.as_bytes(), CODE_LEN)
}

/// The adoption secret: the sticker Wi-Fi password or a generated code.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    fn key(&self) -> [u8; 32] {
        blake3::derive_key(KEY_CONTEXT, self.0.as_bytes())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// 32 random bytes, fresh per exchange.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Challenge([u8; 32]);

impl Challenge {
    pub fn generate() -> Self {
        let mut b = [0u8; 32];
        rand::rng().fill_bytes(&mut b);
        Self(b)
    }
}

impl std::fmt::Debug for Challenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Challenge({})", b64(&self.0))
    }
}

/// A keyed hash over the exchange transcript.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Proof([u8; 32]);

impl std::fmt::Debug for Proof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Proof(..)")
    }
}

/// The tunnel the main router dictates at adoption. Covered by the main
/// router's proof.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TunnelSpec {
    /// The access point's registry name.
    pub access_point: String,
    pub main_listen_port: u16,
    /// The main router's host address on the tunnel.
    pub main_tunnel_addr: String,
    /// The access point's host address on the tunnel.
    pub access_point_tunnel_addr: String,
}

impl TunnelSpec {
    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("TunnelSpec serializes")
    }
}

/// Everything both proofs cover.
#[derive(Clone, Copy, Debug)]
struct Transcript {
    main_key: WgPublicKey,
    access_point_key: WgPublicKey,
    main_challenge: Challenge,
    access_point_challenge: Challenge,
}

impl Transcript {
    fn hasher(&self, key: &[u8; 32]) -> blake3::Hasher {
        let mut h = blake3::Hasher::new_keyed(key);
        h.update(&self.main_challenge.0);
        h.update(&self.access_point_challenge.0);
        h.update(self.main_key.as_bytes());
        h.update(self.access_point_key.as_bytes());
        h
    }

    fn prove(&self, secret: &Secret, label: &[u8], extra: &[u8]) -> Proof {
        let mut h = self.hasher(&secret.key());
        h.update(label);
        h.update(extra);
        Proof(*h.finalize().as_bytes())
    }

    fn verify(&self, secret: &Secret, label: &[u8], extra: &[u8], proof: &Proof) -> bool {
        blake3::Hash::from(self.prove(secret, label, extra).0) == blake3::Hash::from(proof.0)
    }

    fn preshared_key(&self, secret: &Secret) -> [u8; 32] {
        let key = blake3::derive_key(PSK_CONTEXT, secret.0.as_bytes());
        *self.hasher(&key).finalize().as_bytes()
    }
}

/// Main router → candidate: start the exchange.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub main_key: WgPublicKey,
    pub main_challenge: Challenge,
}

/// Candidate → main router: its identity and proof.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub access_point_key: WgPublicKey,
    pub access_point_challenge: Challenge,
    pub proof: Proof,
}

/// Main router → candidate: the tunnel to bring up and the main router's proof
/// over it. Receiving a valid one completes adoption.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Confirm {
    pub tunnel: TunnelSpec,
    pub proof: Proof,
}

/// Why a proof was not accepted.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "reason"
)]
pub enum Reject {
    /// The proof did not verify. `remaining` attempts before a cooldown.
    BadProof { remaining: u8 },
    /// Refusing proofs until `retry_after_secs` have passed.
    Locked { retry_after_secs: u64 },
    /// No exchange in progress to which this message belongs.
    NoExchange,
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadProof { remaining } => {
                write!(f, "wrong password or code ({remaining} attempts left)")
            }
            Self::Locked { retry_after_secs } => {
                write!(
                    f,
                    "too many failed attempts, try again in {retry_after_secs} s"
                )
            }
            Self::NoExchange => f.write_str("no adoption in progress"),
        }
    }
}

/// Five strikes, then a fixed cooldown; cleared by a process restart.
#[derive(Clone, Debug, Default)]
pub struct AttemptGuard {
    failures: u8,
    locked_until: Option<Instant>,
}

impl AttemptGuard {
    /// `Some(reject)` while locked.
    pub fn check(&mut self, now: Instant) -> Option<Reject> {
        match self.locked_until {
            Some(until) if now < until => Some(Reject::Locked {
                retry_after_secs: (until - now).as_secs().max(1),
            }),
            Some(_) => {
                self.locked_until = None;
                self.failures = 0;
                None
            }
            None => None,
        }
    }

    /// Records a bad proof and returns the rejection to send.
    pub fn fail(&mut self, now: Instant) -> Reject {
        self.failures = self.failures.saturating_add(1);
        if self.failures >= ATTEMPT_LIMIT {
            self.locked_until = Some(now + COOLDOWN);
            Reject::Locked {
                retry_after_secs: COOLDOWN.as_secs(),
            }
        } else {
            Reject::BadProof {
                remaining: ATTEMPT_LIMIT - self.failures,
            }
        }
    }

    pub fn succeed(&mut self) {
        self.failures = 0;
        self.locked_until = None;
    }
}

/// The main router's side of one adoption exchange.
#[derive(Debug)]
pub struct Adopter {
    secret: Secret,
    main_key: WgPublicKey,
    main_challenge: Challenge,
}

/// A candidate the main router has accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Adopted {
    pub access_point_key: WgPublicKey,
    pub preshared_key: [u8; 32],
}

impl Adopter {
    pub fn new(secret: Secret, main_key: WgPublicKey) -> Self {
        Self {
            secret,
            main_key,
            main_challenge: Challenge::generate(),
        }
    }

    pub fn hello(&self) -> Hello {
        Hello {
            main_key: self.main_key,
            main_challenge: self.main_challenge,
        }
    }

    /// Checks the candidate's proof. `Ok` carries the confirmation to send and
    /// what to pin; `Err` carries the rejection to send. A fresh challenge is
    /// drawn after either outcome, so no proof can be presented twice.
    pub fn accept(
        &mut self,
        offer: &Offer,
        tunnel: TunnelSpec,
        guard: &mut AttemptGuard,
        now: Instant,
    ) -> Result<(Confirm, Adopted), Reject> {
        if let Some(r) = guard.check(now) {
            return Err(r);
        }
        let t = Transcript {
            main_key: self.main_key,
            access_point_key: offer.access_point_key,
            main_challenge: self.main_challenge,
            access_point_challenge: offer.access_point_challenge,
        };
        self.main_challenge = Challenge::generate();
        if !t.verify(&self.secret, LABEL_ACCESS_POINT, &[], &offer.proof) {
            return Err(guard.fail(now));
        }
        guard.succeed();
        let proof = t.prove(&self.secret, LABEL_MAIN, &tunnel.bytes());
        Ok((
            Confirm { tunnel, proof },
            Adopted {
                access_point_key: offer.access_point_key,
                preshared_key: t.preshared_key(&self.secret),
            },
        ))
    }
}

/// The candidate's side of one adoption exchange.
#[derive(Debug)]
pub struct Responder {
    secret: Secret,
    access_point_key: WgPublicKey,
    pending: Option<Transcript>,
}

/// The main router a candidate has accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptedBy {
    pub main_key: WgPublicKey,
    pub preshared_key: [u8; 32],
    pub tunnel: TunnelSpec,
}

impl Responder {
    pub fn new(secret: Secret, access_point_key: WgPublicKey) -> Self {
        Self {
            secret,
            access_point_key,
            pending: None,
        }
    }

    /// Answers a main router's hello. Only the latest hello can be confirmed.
    pub fn respond(
        &mut self,
        hello: &Hello,
        guard: &mut AttemptGuard,
        now: Instant,
    ) -> Result<Offer, Reject> {
        if let Some(r) = guard.check(now) {
            return Err(r);
        }
        let t = Transcript {
            main_key: hello.main_key,
            access_point_key: self.access_point_key,
            main_challenge: hello.main_challenge,
            access_point_challenge: Challenge::generate(),
        };
        let proof = t.prove(&self.secret, LABEL_ACCESS_POINT, &[]);
        self.pending = Some(t);
        Ok(Offer {
            access_point_key: self.access_point_key,
            access_point_challenge: t.access_point_challenge,
            proof,
        })
    }

    /// Checks the main router's proof against the hello most recently answered.
    pub fn confirm(
        &mut self,
        confirm: &Confirm,
        guard: &mut AttemptGuard,
        now: Instant,
    ) -> Result<AdoptedBy, Reject> {
        if let Some(r) = guard.check(now) {
            return Err(r);
        }
        let Some(t) = self.pending.take() else {
            return Err(Reject::NoExchange);
        };
        if !t.verify(
            &self.secret,
            LABEL_MAIN,
            &confirm.tunnel.bytes(),
            &confirm.proof,
        ) {
            return Err(guard.fail(now));
        }
        guard.succeed();
        Ok(AdoptedBy {
            main_key: t.main_key,
            preshared_key: t.preshared_key(&self.secret),
            tunnel: confirm.tunnel.clone(),
        })
    }
}

pub(super) fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn from_b64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
    let s = String::deserialize(d)?;
    let v = base64::engine::general_purpose::STANDARD
        .decode(&s)
        .map_err(serde::de::Error::custom)?;
    v.try_into()
        .map_err(|_| serde::de::Error::custom("expected 32 bytes"))
}

macro_rules! base64_bytes {
    ($t:ident) => {
        impl Serialize for $t {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&b64(&self.0))
            }
        }
        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                from_b64(d).map($t)
            }
        }
    };
}
base64_bytes!(Challenge);
base64_bytes!(Proof);

impl Serialize for WgPublicKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&b64(self.as_bytes()))
    }
}

impl<'de> Deserialize<'de> for WgPublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        from_b64(d).map(|b| WgPublicKey::try_from(b.to_vec()).expect("32 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wg::WgKey;

    fn keys() -> (WgPublicKey, WgPublicKey) {
        (
            WgKey::generate().public_key(),
            WgKey::generate().public_key(),
        )
    }

    fn spec() -> TunnelSpec {
        TunnelSpec {
            access_point: "s1".into(),
            main_listen_port: 51900,
            main_tunnel_addr: "10.42.1.1".into(),
            access_point_tunnel_addr: "10.42.1.2".into(),
        }
    }

    struct Pair {
        main: Adopter,
        ap: Responder,
        mg: AttemptGuard,
        ag: AttemptGuard,
    }

    fn pair(main_secret: &str, ap_secret: &str) -> Pair {
        let (mk, ak) = keys();
        Pair {
            main: Adopter::new(Secret::new(main_secret), mk),
            ap: Responder::new(Secret::new(ap_secret), ak),
            mg: AttemptGuard::default(),
            ag: AttemptGuard::default(),
        }
    }

    #[test]
    fn matching_secrets_adopt_both_ways_and_agree_on_the_psk() {
        let mut p = pair("sticker", "sticker");
        let now = Instant::now();
        let offer = p.ap.respond(&p.main.hello(), &mut p.ag, now).unwrap();
        let (confirm, adopted) = p.main.accept(&offer, spec(), &mut p.mg, now).unwrap();
        assert_eq!(adopted.access_point_key, p.ap.access_point_key);
        let by = p.ap.confirm(&confirm, &mut p.ag, now).unwrap();
        assert_eq!(by.main_key, p.main.main_key);
        assert_eq!(by.preshared_key, adopted.preshared_key);
        assert_eq!(by.tunnel, spec());
    }

    #[test]
    fn wrong_secret_is_rejected_by_the_main_router() {
        let mut p = pair("sticker", "guess");
        let now = Instant::now();
        let offer = p.ap.respond(&p.main.hello(), &mut p.ag, now).unwrap();
        assert_eq!(
            p.main.accept(&offer, spec(), &mut p.mg, now).unwrap_err(),
            Reject::BadProof { remaining: 4 }
        );
    }

    #[test]
    fn fake_main_router_is_rejected_by_the_access_point() {
        let mut p = pair("guess", "sticker");
        let now = Instant::now();
        p.ap.respond(&p.main.hello(), &mut p.ag, now).unwrap();
        let bogus = Confirm {
            tunnel: spec(),
            proof: Proof([7; 32]),
        };
        assert_eq!(
            p.ap.confirm(&bogus, &mut p.ag, now).unwrap_err(),
            Reject::BadProof { remaining: 4 }
        );
    }

    #[test]
    fn tampered_tunnel_fails_the_main_router_proof() {
        let mut p = pair("sticker", "sticker");
        let now = Instant::now();
        let offer = p.ap.respond(&p.main.hello(), &mut p.ag, now).unwrap();
        let (mut confirm, _) = p.main.accept(&offer, spec(), &mut p.mg, now).unwrap();
        confirm.tunnel.main_listen_port += 1;
        assert!(p.ap.confirm(&confirm, &mut p.ag, now).is_err());
    }

    #[test]
    fn the_two_proofs_are_not_interchangeable() {
        let (mk, ak) = keys();
        let t = Transcript {
            main_key: mk,
            access_point_key: ak,
            main_challenge: Challenge::generate(),
            access_point_challenge: Challenge::generate(),
        };
        let s = Secret::new("sticker");
        assert_ne!(
            t.prove(&s, LABEL_ACCESS_POINT, &[]),
            t.prove(&s, LABEL_MAIN, &[])
        );
        assert!(!t.verify(&s, LABEL_MAIN, &[], &t.prove(&s, LABEL_ACCESS_POINT, &[])));
    }

    #[test]
    fn substituted_key_fails_the_proof() {
        let mut p = pair("sticker", "sticker");
        let now = Instant::now();
        let mut offer = p.ap.respond(&p.main.hello(), &mut p.ag, now).unwrap();
        offer.access_point_key = WgKey::generate().public_key();
        assert!(p.main.accept(&offer, spec(), &mut p.mg, now).is_err());
    }

    #[test]
    fn replayed_offer_fails_on_the_fresh_challenge() {
        let mut p = pair("sticker", "sticker");
        let now = Instant::now();
        let offer = p.ap.respond(&p.main.hello(), &mut p.ag, now).unwrap();
        assert!(p.main.accept(&offer, spec(), &mut p.mg, now).is_ok());
        assert!(p.main.accept(&offer, spec(), &mut p.mg, now).is_err());
    }

    #[test]
    fn confirm_without_a_pending_hello_is_rejected() {
        let mut p = pair("sticker", "sticker");
        let c = Confirm {
            tunnel: spec(),
            proof: Proof([0; 32]),
        };
        assert_eq!(
            p.ap.confirm(&c, &mut p.ag, Instant::now()).unwrap_err(),
            Reject::NoExchange
        );
    }

    #[test]
    fn five_failures_lock_for_sixty_seconds() {
        let mut g = AttemptGuard::default();
        let t0 = Instant::now();
        for remaining in (1..ATTEMPT_LIMIT).rev() {
            assert_eq!(g.fail(t0), Reject::BadProof { remaining });
        }
        assert_eq!(
            g.fail(t0),
            Reject::Locked {
                retry_after_secs: 60
            }
        );
        assert_eq!(
            g.check(t0 + Duration::from_secs(30)),
            Some(Reject::Locked {
                retry_after_secs: 30
            })
        );
        assert_eq!(g.check(t0 + COOLDOWN), None);
        assert_eq!(g.fail(t0 + COOLDOWN), Reject::BadProof { remaining: 4 });
    }

    #[test]
    fn locked_main_router_refuses_before_checking() {
        let mut p = pair("sticker", "sticker");
        let t0 = Instant::now();
        for _ in 0..ATTEMPT_LIMIT {
            p.mg.fail(t0);
        }
        let offer = p.ap.respond(&p.main.hello(), &mut p.ag, t0).unwrap();
        assert!(matches!(
            p.main.accept(&offer, spec(), &mut p.mg, t0).unwrap_err(),
            Reject::Locked { .. }
        ));
        let later = t0 + COOLDOWN;
        let offer = p.ap.respond(&p.main.hello(), &mut p.ag, later).unwrap();
        assert!(p.main.accept(&offer, spec(), &mut p.mg, later).is_ok());
    }

    #[test]
    fn a_success_clears_the_strike_count() {
        let mut p = pair("sticker", "sticker");
        let now = Instant::now();
        p.mg.fail(now);
        p.mg.fail(now);
        let offer = p.ap.respond(&p.main.hello(), &mut p.ag, now).unwrap();
        p.main.accept(&offer, spec(), &mut p.mg, now).unwrap();
        assert_eq!(p.mg.fail(now), Reject::BadProof { remaining: 4 });
    }

    #[test]
    fn messages_round_trip_through_json() {
        let (mk, ak) = keys();
        let hello = Hello {
            main_key: mk,
            main_challenge: Challenge::generate(),
        };
        let s = serde_json::to_string(&hello).unwrap();
        assert_eq!(serde_json::from_str::<Hello>(&s).unwrap(), hello);
        let offer = Offer {
            access_point_key: ak,
            access_point_challenge: Challenge::generate(),
            proof: Proof([9; 32]),
        };
        let s = serde_json::to_string(&offer).unwrap();
        assert_eq!(serde_json::from_str::<Offer>(&s).unwrap(), offer);
        let confirm = Confirm {
            tunnel: spec(),
            proof: Proof([1; 32]),
        };
        let s = serde_json::to_string(&confirm).unwrap();
        assert_eq!(serde_json::from_str::<Confirm>(&s).unwrap(), confirm);
        let r = Reject::Locked {
            retry_after_secs: 42,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"reason":"locked","retryAfterSecs":42}"#);
        assert_eq!(serde_json::from_str::<Reject>(&s).unwrap(), r);
        assert!(serde_json::from_str::<Proof>("\"AAAA\"").is_err());
    }

    #[test]
    fn generated_code_is_long_and_unambiguous() {
        let code = generate_code();
        assert_eq!(code.len(), CODE_LEN);
        assert!(code
            .bytes()
            .all(|b| crate::PASSWORD_CHARS_ALNUM.as_bytes().contains(&b)));
        assert_ne!(code, generate_code());
    }

    #[test]
    fn secret_never_prints() {
        assert_eq!(format!("{:?}", Secret::new("hunter2")), "Secret(..)");
        let a = Adopter::new(Secret::new("hunter2"), keys().0);
        assert!(!format!("{a:?}").contains("hunter2"));
    }
}
