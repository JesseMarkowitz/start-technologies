//! The adoption proof (design D24): how a candidate proves it is the unit whose
//! sticker password the admin typed at the Core, and how the Core proves it
//! holds that password too, without the password crossing the backhaul.
//!
//! Transport-agnostic. The Core runs an [`Adopter`] per candidate; the candidate
//! runs a [`Responder`]. Three messages:
//!
//! 1. Core → candidate: [`Hello`], the Core's WireGuard public key and a fresh
//!    challenge.
//! 2. Candidate → Core: [`Offer`], its own public key, its own challenge and its
//!    proof.
//! 3. Core → candidate: [`Confirm`], the tunnel the satellite is to bring up and
//!    the Core's proof over it, or [`Reject`].
//!
//! Each proof is a keyed BLAKE3 hash over both challenges and both public keys,
//! keyed by the adoption secret, with a direction label so the two proofs can
//! never stand in for each other; the Core's also covers the tunnel it dictates.
//! A replayed proof fails on the fresh challenge and a substituted key fails on
//! the transcript. The tunnel's pre-shared key is derived from the same material
//! on both sides and never sent. Both sides refuse after [`ATTEMPT_LIMIT`] bad
//! proofs for [`COOLDOWN`].

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

const KEY_CONTEXT: &str = "StartWRT satellite adoption v1 proof";
const PSK_CONTEXT: &str = "StartWRT satellite adoption v1 preshared key";
const LABEL_SATELLITE: &[u8] = b"satellite->core";
const LABEL_CORE: &[u8] = b"core->satellite";

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

/// The tunnel the Core dictates at adoption. Covered by the Core's proof.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TunnelSpec {
    /// The satellite's registry name.
    pub satellite: String,
    /// The Core profile interface the tunnel carries.
    pub profile: String,
    pub core_listen_port: u16,
    /// The Core's host address on the tunnel.
    pub core_tunnel_addr: String,
    /// The satellite's host address on the tunnel.
    pub satellite_tunnel_addr: String,
    /// What the satellite routes through the tunnel.
    pub allowed_ips: Vec<String>,
}

impl TunnelSpec {
    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("TunnelSpec serializes")
    }
}

/// Everything both proofs cover.
#[derive(Clone, Copy, Debug)]
struct Transcript {
    core_key: WgPublicKey,
    satellite_key: WgPublicKey,
    core_challenge: Challenge,
    satellite_challenge: Challenge,
}

impl Transcript {
    fn hasher(&self, key: &[u8; 32]) -> blake3::Hasher {
        let mut h = blake3::Hasher::new_keyed(key);
        h.update(&self.core_challenge.0);
        h.update(&self.satellite_challenge.0);
        h.update(self.core_key.as_bytes());
        h.update(self.satellite_key.as_bytes());
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

/// Core → candidate: start the exchange.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub core_key: WgPublicKey,
    pub core_challenge: Challenge,
}

/// Candidate → Core: its identity and proof.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub satellite_key: WgPublicKey,
    pub satellite_challenge: Challenge,
    pub proof: Proof,
}

/// Core → candidate: the tunnel to bring up and the Core's proof over it.
/// Receiving a valid one completes adoption.
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

/// The Core's side of one adoption exchange.
#[derive(Debug)]
pub struct Adopter {
    secret: Secret,
    core_key: WgPublicKey,
    core_challenge: Challenge,
}

/// A candidate the Core has accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Adopted {
    pub satellite_key: WgPublicKey,
    pub preshared_key: [u8; 32],
}

impl Adopter {
    pub fn new(secret: Secret, core_key: WgPublicKey) -> Self {
        Self {
            secret,
            core_key,
            core_challenge: Challenge::generate(),
        }
    }

    pub fn hello(&self) -> Hello {
        Hello {
            core_key: self.core_key,
            core_challenge: self.core_challenge,
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
            core_key: self.core_key,
            satellite_key: offer.satellite_key,
            core_challenge: self.core_challenge,
            satellite_challenge: offer.satellite_challenge,
        };
        self.core_challenge = Challenge::generate();
        if !t.verify(&self.secret, LABEL_SATELLITE, &[], &offer.proof) {
            return Err(guard.fail(now));
        }
        guard.succeed();
        let proof = t.prove(&self.secret, LABEL_CORE, &tunnel.bytes());
        Ok((
            Confirm { tunnel, proof },
            Adopted {
                satellite_key: offer.satellite_key,
                preshared_key: t.preshared_key(&self.secret),
            },
        ))
    }
}

/// The candidate's side of one adoption exchange.
#[derive(Debug)]
pub struct Responder {
    secret: Secret,
    satellite_key: WgPublicKey,
    pending: Option<Transcript>,
}

/// The Core a candidate has accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptedBy {
    pub core_key: WgPublicKey,
    pub preshared_key: [u8; 32],
    pub tunnel: TunnelSpec,
}

impl Responder {
    pub fn new(secret: Secret, satellite_key: WgPublicKey) -> Self {
        Self {
            secret,
            satellite_key,
            pending: None,
        }
    }

    /// Answers a Core's hello. Only the latest hello can be confirmed.
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
            core_key: hello.core_key,
            satellite_key: self.satellite_key,
            core_challenge: hello.core_challenge,
            satellite_challenge: Challenge::generate(),
        };
        let proof = t.prove(&self.secret, LABEL_SATELLITE, &[]);
        self.pending = Some(t);
        Ok(Offer {
            satellite_key: self.satellite_key,
            satellite_challenge: t.satellite_challenge,
            proof,
        })
    }

    /// Checks the Core's proof against the hello most recently answered.
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
            LABEL_CORE,
            &confirm.tunnel.bytes(),
            &confirm.proof,
        ) {
            return Err(guard.fail(now));
        }
        guard.succeed();
        Ok(AdoptedBy {
            core_key: t.core_key,
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
            satellite: "s1".into(),
            profile: "guest".into(),
            core_listen_port: 51900,
            core_tunnel_addr: "10.42.1.1".into(),
            satellite_tunnel_addr: "10.42.1.2".into(),
            allowed_ips: vec!["0.0.0.0/0".into()],
        }
    }

    struct Pair {
        core: Adopter,
        sat: Responder,
        cg: AttemptGuard,
        sg: AttemptGuard,
    }

    fn pair(core_secret: &str, sat_secret: &str) -> Pair {
        let (ck, sk) = keys();
        Pair {
            core: Adopter::new(Secret::new(core_secret), ck),
            sat: Responder::new(Secret::new(sat_secret), sk),
            cg: AttemptGuard::default(),
            sg: AttemptGuard::default(),
        }
    }

    #[test]
    fn matching_secrets_adopt_both_ways_and_agree_on_the_psk() {
        let mut p = pair("sticker", "sticker");
        let now = Instant::now();
        let offer = p.sat.respond(&p.core.hello(), &mut p.sg, now).unwrap();
        let (confirm, adopted) = p.core.accept(&offer, spec(), &mut p.cg, now).unwrap();
        assert_eq!(adopted.satellite_key, p.sat.satellite_key);
        let by = p.sat.confirm(&confirm, &mut p.sg, now).unwrap();
        assert_eq!(by.core_key, p.core.core_key);
        assert_eq!(by.preshared_key, adopted.preshared_key);
        assert_eq!(by.tunnel, spec());
    }

    #[test]
    fn wrong_secret_is_rejected_by_the_core() {
        let mut p = pair("sticker", "guess");
        let now = Instant::now();
        let offer = p.sat.respond(&p.core.hello(), &mut p.sg, now).unwrap();
        assert_eq!(
            p.core.accept(&offer, spec(), &mut p.cg, now).unwrap_err(),
            Reject::BadProof { remaining: 4 }
        );
    }

    #[test]
    fn fake_core_is_rejected_by_the_satellite() {
        let mut p = pair("guess", "sticker");
        let now = Instant::now();
        p.sat.respond(&p.core.hello(), &mut p.sg, now).unwrap();
        let bogus = Confirm {
            tunnel: spec(),
            proof: Proof([7; 32]),
        };
        assert_eq!(
            p.sat.confirm(&bogus, &mut p.sg, now).unwrap_err(),
            Reject::BadProof { remaining: 4 }
        );
    }

    #[test]
    fn tampered_tunnel_fails_the_core_proof() {
        let mut p = pair("sticker", "sticker");
        let now = Instant::now();
        let offer = p.sat.respond(&p.core.hello(), &mut p.sg, now).unwrap();
        let (mut confirm, _) = p.core.accept(&offer, spec(), &mut p.cg, now).unwrap();
        confirm.tunnel.allowed_ips = vec!["10.0.0.0/8".into()];
        assert!(p.sat.confirm(&confirm, &mut p.sg, now).is_err());
    }

    #[test]
    fn the_two_proofs_are_not_interchangeable() {
        let (ck, sk) = keys();
        let t = Transcript {
            core_key: ck,
            satellite_key: sk,
            core_challenge: Challenge::generate(),
            satellite_challenge: Challenge::generate(),
        };
        let s = Secret::new("sticker");
        assert_ne!(
            t.prove(&s, LABEL_SATELLITE, &[]),
            t.prove(&s, LABEL_CORE, &[])
        );
        assert!(!t.verify(&s, LABEL_CORE, &[], &t.prove(&s, LABEL_SATELLITE, &[])));
    }

    #[test]
    fn substituted_key_fails_the_proof() {
        let mut p = pair("sticker", "sticker");
        let now = Instant::now();
        let mut offer = p.sat.respond(&p.core.hello(), &mut p.sg, now).unwrap();
        offer.satellite_key = WgKey::generate().public_key();
        assert!(p.core.accept(&offer, spec(), &mut p.cg, now).is_err());
    }

    #[test]
    fn replayed_offer_fails_on_the_fresh_challenge() {
        let mut p = pair("sticker", "sticker");
        let now = Instant::now();
        let offer = p.sat.respond(&p.core.hello(), &mut p.sg, now).unwrap();
        assert!(p.core.accept(&offer, spec(), &mut p.cg, now).is_ok());
        assert!(p.core.accept(&offer, spec(), &mut p.cg, now).is_err());
    }

    #[test]
    fn confirm_without_a_pending_hello_is_rejected() {
        let mut p = pair("sticker", "sticker");
        let c = Confirm {
            tunnel: spec(),
            proof: Proof([0; 32]),
        };
        assert_eq!(
            p.sat.confirm(&c, &mut p.sg, Instant::now()).unwrap_err(),
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
    fn locked_core_refuses_before_checking() {
        let mut p = pair("sticker", "sticker");
        let t0 = Instant::now();
        for _ in 0..ATTEMPT_LIMIT {
            p.cg.fail(t0);
        }
        let offer = p.sat.respond(&p.core.hello(), &mut p.sg, t0).unwrap();
        assert!(matches!(
            p.core.accept(&offer, spec(), &mut p.cg, t0).unwrap_err(),
            Reject::Locked { .. }
        ));
        let later = t0 + COOLDOWN;
        let offer = p.sat.respond(&p.core.hello(), &mut p.sg, later).unwrap();
        assert!(p.core.accept(&offer, spec(), &mut p.cg, later).is_ok());
    }

    #[test]
    fn a_success_clears_the_strike_count() {
        let mut p = pair("sticker", "sticker");
        let now = Instant::now();
        p.cg.fail(now);
        p.cg.fail(now);
        let offer = p.sat.respond(&p.core.hello(), &mut p.sg, now).unwrap();
        p.core.accept(&offer, spec(), &mut p.cg, now).unwrap();
        assert_eq!(p.cg.fail(now), Reject::BadProof { remaining: 4 });
    }

    #[test]
    fn messages_round_trip_through_json() {
        let (ck, sk) = keys();
        let hello = Hello {
            core_key: ck,
            core_challenge: Challenge::generate(),
        };
        let s = serde_json::to_string(&hello).unwrap();
        assert_eq!(serde_json::from_str::<Hello>(&s).unwrap(), hello);
        let offer = Offer {
            satellite_key: sk,
            satellite_challenge: Challenge::generate(),
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
