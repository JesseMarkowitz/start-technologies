# Satellite auth and issue #3670 — what is being retired, and what to build instead

> **Superseded by the filed issue (2026-09-21).** Research note. Its conclusions are folded into the issue attachment (`SupportingEvidenceForSatelliteRouter.md`, §3 implementation concerns — the management RPC boundary). Kept for the detail behind them; the attachment is authoritative.

Working note. Explains what StartWRT's authentication looks like today, what #3670 plans to replace
it with, and where satellite pairing auth should attach. Code references are against `master` as of
2026-09-20.

---

## 1. What StartWRT authentication is today

One middleware, `SessionAuth`, in `projects/start-wrt/backend/ctrl/src/middleware/auth.rs`. It proves
identity three ways, and they are checked in this order inside `process_rpc_request`:

| #   | Mechanism                  | Where                                | What it actually trusts                                                                  |
| --- | -------------------------- | ------------------------------------ | ---------------------------------------------------------------------------------------- |
| 1   | **`no_auth` metadata**     | `:144`                               | The endpoint declared itself public.                                                     |
| 2   | **Loopback bypass**        | `:144` (`self.is_loopback`)          | **Any peer whose source IP is loopback, with no credential at all.**                     |
| 3   | **Local auth cookie**      | `:108`, `validate_local_auth_cookie` | Possession of `/run/startwrt/rpc.authcookie` — i.e. root on the box, typically over SSH. |
| 4   | **Browser session cookie** | `:98`, `validate_session`            | A `session=<token>` cookie minted at login and held in `/etc/startwrt/sessions.json`.    |

Two structural facts matter for the satellite work.

**It is a single monolithic middleware.** `SessionAuth` is one struct with one `process_rpc_request`
that does all four checks inline. There is no composition, no way to add a fifth mechanism except by
adding another branch to that function.

**It has no notion of _who_ you are or _where_ you came from.** The result is binary: authorized or
not. There is no principal, no scope, no record that "this request arrived over satellite S1's
management tunnel." Every authorized caller is a full admin. That is adequate for a router with one
admin and a CLI; it is not adequate for a paired peer that should be allowed to do four things and
nothing else.

The loopback bypass is the part that draws the most fire. `is_loopback` is computed in
`process_http_request` (`:118-129`) from the TCP peer address, and at `:144` it short-circuits
everything. The daemon listens on all interfaces, so any local process — and any SSRF sink that can
emit a request to `127.0.0.1:80` — becomes an unauthenticated router admin. It is also redundant:
`startwrt-cli` already sends the `local=` cookie, so the intended root-CLI path is authenticated by
the token regardless.

---

## 2. What #3670 retires

#3670 ("adopt start-os signature auth model") is a tracking issue to delete that whole stack and
adopt the `start-core` model StartOS already runs. Specifically:

- **Delete the loopback bypass** (`middleware/auth.rs:144`). Explicitly shippable on its own, ahead
  of everything else, because the `local=` cookie already covers the intended callers.
- **Replace the browser session cookie** with per-request Ed25519 signatures. The browser holds a
  non-extractable WebCrypto key enrolled at login and signs a commitment over timestamp + nonce +
  body hash + server identity. Nothing replayable crosses the wire, and because WebCrypto signing
  requires a secure context, authenticated RPC cannot happen over cleartext `:80` at all.
- **Move the local cookie to `Authorization: Bearer`**, adopting `start-core`'s `LocalAuth`.
- **Give the token file explicit ownership** (`root:startos`-style) rather than today's implicit
  root-only mode.
- **Retire the session store** once signature auth covers the UI.

So: `middleware/auth.rs` as it exists is scheduled for deletion, not extension.

---

## 3. What replaces it, concretely

The target is `shared-libs/crates/start-core/src/middleware/auth/`, which is three files:

```
auth/mod.rs        — Auth<C>: the composition
auth/local.rs      — LocalAuth: Authorization: Bearer <token file>
auth/signature.rs  — SignatureAuth: per-request Ed25519
```

`Auth<C>` is a **vector of middlewares evaluated with OR semantics**
(`auth/mod.rs`, `process_rpc_request`):

```rust
pub struct Auth<C: Context>(Vec<DynMiddleware<C>>);

impl<C: LocalAuthContext> Auth<C> {
    pub fn with_local_auth(mut self) -> Self { … }
}
impl<C: SignatureAuthContext> Auth<C> {
    pub fn with_signature_auth(mut self) -> Self { … }
}
```

The loop tries each middleware in turn; **the first one that returns `Ok` authorizes the request**,
and an error is only retained if the endpoint's metadata says `authenticated: true`. Note the
polarity flip from StartWRT: start-core's metadata field is `authenticated` (default _true_, via
`const_true`), where StartWRT's is `no_auth` (default false). Same meaning, inverted default, and the
start-core direction is the safer one — an endpoint is protected unless it opts out.

Each mechanism is gated by a **context trait**, which is how a product declares it supports that
mechanism and supplies its own constants. `LocalAuthContext` is the model to copy:

```rust
pub trait LocalAuthContext: Context {
    const LOCAL_AUTH_COOKIE_PATH: &str;
    const LOCAL_AUTH_COOKIE_OWNERSHIP: &str;
    fn init_auth_cookie() -> … // creates the file 0640 and chowns it
}

impl LocalAuthContext for RpcContext {
    const LOCAL_AUTH_COOKIE_PATH: &str = "/run/startos/rpc.authcookie";
    const LOCAL_AUTH_COOKIE_OWNERSHIP: &str = "root:startos";
}
```

One more detail worth copying: `local.rs` has an `is_loopback(url: &Url)` helper, but it is a
**client-side** decision about where the CLI may _send_ the secret — not a server-side trust check.
There is no server-side loopback trust anywhere in `start-core`. Local admin is granted by
_possession of the token file_, which requires read permission, which is exactly what an
SSH-authenticated admin has. "Keep admin over SSH" and "drop loopback admin" turn out to be the same
mechanism, not competing goals.

---

## 4. What the satellite solution should implement

### 4.1 The shape: a fourth middleware, not a fourth branch

Satellite auth should be `SatelliteAuth` — a middleware in the same composition, added with
`.with_satellite_auth()` and gated on a `SatelliteAuthContext` trait carrying the registry path and
the tunnel's address block. It is not a branch inside `SessionAuth`, and it is not a parallel
hand-rolled path beside it.

Three reasons this is the right call rather than a stylistic preference:

1. **The file it would otherwise live in is being deleted.** Building into `SessionAuth` guarantees a
   rewrite, and guarantees the satellite path is the messy leftover when the rest migrates.
2. **OR-composition is exactly the semantics needed.** A satellite request should authorize _without_
   a session and _without_ the local token, and today's function would need reordering to express
   that cleanly.
3. **The security review gets much cheaper.** A self-contained middleware with one entry point is
   reviewable in isolation. A fifth branch in a 100-line `if` ladder is not.

### 4.2 The mechanism

A per-pairing bearer token, **bound to the management tunnel**, with the binding checked server-side:

- **Credential.** A high-entropy token issued at pairing, stored at the Core against the satellite's
  pinned Ed25519 / WireGuard public key, and stored on the satellite `0600`. Sent as
  `Authorization: Bearer <token>`, matching `LocalAuth`'s wire format so the two look alike.
- **Source binding — the part that carries the security.** The token alone must not be sufficient.
  The request must also have _arrived on that satellite's management tunnel interface_, with a source
  address inside the transit block the Core allocated that satellite. This is checkable from
  `TcpMetadata` (the same extension `SessionAuth` reads at `:118` to compute `is_loopback`) plus the
  satellite registry. A token stolen from a physically-stolen satellite is then useless from anywhere
  else on the network.
- **Direction matters.** There are two channels, and they are not symmetric:
  - _Core → satellite_ (config apply). The satellite authenticates the Core. This is where the
    satellite pins the Core's identity at pairing and accepts nothing else afterward.
  - _Satellite → Core_ (device reports, port-forward relay, sync acks). The Core authenticates the
    satellite. This is the channel the token above protects, and it is the new direction of trust —
    it did not exist when the original threat model was written.
- **Scope, not admin.** This is the piece today's auth cannot express at all. A satellite token must
  authorize a **named allowlist** — `satellite.sync-ack`, `satellite.report-devices`,
  `satellite.request-forward`, a heartbeat — and nothing else. It must not be able to call
  `profiles.*`, `wifi.*`, `wan.*` or `published-ports.*`. Since `Auth<C>` returns a bare `Ok(())`
  today, expressing scope means either carrying a principal forward (a change to the shared
  composition, worth proposing upstream) or having `SatelliteAuth` reject any method outside its
  allowlist before it authorizes. **The second is the pragmatic v1**: the middleware sees the
  `RpcRequest` and can check the method name itself.
- **Revocation.** Unpair deletes the token at the Core and drops the tunnels. Because the binding is
  to a live tunnel, dropping the tunnel is itself most of the revocation.
- **Firewall as defence in depth.** The satellite's config-apply RPC should additionally be firewalled
  to the tunnel interface, so a misconfigured middleware is not the only thing standing between the
  LAN and remote configuration.

### 4.3 Sequencing

- **#3670's loopback deletion should land first**, and is independent of everything else. It is
  worth waiting for or contributing: a satellite network doubles the number of boxes on which "any
  local process is admin" is true, and a satellite sitting in a garage is a more plausible place to
  get a foothold than the Core.
- **Satellite auth should target the start-core composition**, which means it either follows #3670's
  StartWRT migration or lands alongside it. Building against the current stack and migrating later is
  the option that costs the most total work.
- **Raise one question upstream:** does `Auth<C>` want to carry a principal (who authorized, with
  what scope) rather than returning bare `Ok(())`? StartWRT's satellite is the first caller that is
  authenticated but _not_ an admin. StartOS may not need it, but the answer determines whether
  StartWRT implements scope inside its own middleware or the shared one grows the concept. Asking on
  #3670 before writing code is cheap and makes the satellite work a contribution to the shared crate
  rather than a divergence from it.

---

## 5. One-paragraph summary

#3670 deletes StartWRT's hand-rolled `SessionAuth` — the loopback bypass, the session cookie and the
`local=` cookie — and replaces it with `start-core`'s composed `Auth<C>`, an OR-evaluated vector of
middlewares in which the browser proves itself with a per-request Ed25519 signature and local
processes with `Authorization: Bearer` from a root-owned token file. Satellite pairing auth should be
a fifth mechanism in that composition, not a fifth branch in the code being deleted: a per-pairing
bearer token that is only accepted when the request also arrives on that satellite's management
tunnel from an address the Core allocated it, restricted to a named allowlist of satellite endpoints
rather than granting admin. The loopback deletion is worth landing first on its own merits, and
whether the shared `Auth<C>` should carry a principal and scope is a question to raise on #3670
before writing the satellite path.
