# Satellite Router — Questions Parked for Implementation

Working notes. **Not part of the issue package** (`satellite-router-issue-form.md` +
`SupportingEvidenceForSatelliteRouter.md`) — these are decisions to settle when the work is
designed and coded, recorded here so they are not lost between raising the issue and building it.

---

## 1. Backup, restore, and a satellite that is ahead of its Core

Raised 2026-09-21.

Config sync carries a monotonic generation number, and a satellite refuses a snapshot older than
the one it has applied. That anti-rollback rule collides with restoring a Core from backup:

> The Core fails. A satellite is on generation 26. The Core's most recent backup is from
> generation 25. The restored Core pushes 25; the satellite refuses it as stale, and the two are
> stuck.

The likely resolution, to be confirmed rather than assumed:

- **A satellite holds no authoritative state.** Everything it has came from the Core. So the
  recovery path is to factory-reset the satellites, restore the Core, and re-pair — after which the
  system is exactly as it was, minus whatever the Core lost between its last backup and the failure.
- **The Core must stay authoritative even when it is behind.** Changes made after the last backup
  are lost, by design. A satellite holding a newer generation must therefore not be able to rejoin a
  Core restored from an older one; it is reset and re-paired instead.
- If the backup is recent enough to be in sync, satellites should simply reconnect when the Core
  comes back, exactly as they would after a power cut.

Open: whether a satellite is worth backing up at all (probably not — a failed satellite is replaced,
comes up blank and syncs), and how the "you must reset your satellites" state is detected and
communicated rather than presenting as a silent refusal.

---

## 2. Firmware upgrades across a Core and its satellites

Raised 2026-09-21.

Proposed approach, security-first and deliberately blunt:

1. Upgrade the **Core** first.
2. Satellites running the older firmware drop off as incompatible — the version check is explicit,
   not a best-effort negotiation.
3. Each satellite is then flashed, comes up factory-reset and blank, and is re-paired to the Core.

The cost is real and should be stated plainly: **every upgrade is a whole-fleet event**, and every
satellite goes through initial enrollment again. That is painful in proportion to the number of
satellites, which today is one.

Open: whether a narrower compatibility window (a satellite may lag the Core by one minor version, or
the sync payload is versioned independently of the firmware) buys enough to be worth the extra
surface. Alternate approaches welcome; the security argument for the blunt version is that no code
path has to be correct across a version skew.

---

## 3. Implementation concerns moved out of the issue attachment

Recorded 2026-09-21. These were concerns 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 17, 18 and 21 in
`SupportingEvidenceForSatelliteRouter.md`. They are real work with known approaches; none of them
bears on whether the feature should be built, so the attachment now names them in one line each and
the full text lives here. Numbering below is the number each carried in the attachment.

### 3. Tunnel MTU and MSS clamping.

There is no MSS clamp on the tunnel ingress path and no `mtu_fix` on the profile zones, so large TCP
flows from satellite hosts would black-hole while pings succeed — the failure that presents as "some
websites don't load." Set a correct tunnel MTU and/or `mtu_fix` (reuse the pattern in
`ensure_vpn_outbound_zone`), and put a large-payload transfer in the hardware suite, not just a ping.

### 4. WAN-less egress on the satellite — the top technical risk.

A satellite has no WAN, but several paths assume one exists: `vpn_client.rs:1341
rewrite_vpn_chain_routes` pins a VPN endpoint `/32` via the target interface assuming a base uplink;
`profiles.rs:2232 rewrite_routing` builds per-VLAN policy tables with `unreachable` kill-switch
fallbacks that assume a WAN default. Each needs a defined WAN-less behavior: pin the Core tunnel
endpoint via the local transit link, re-point DNS at the Core resolver, and decide what the
kill-switch _means_ on a router whose only uplink is a tunnel.

**There is now a documented upstream pattern for exactly this.** #4006 documented the policy-routing
rule ladder and its invariants. The one named `wg-transport` is the satellite's problem solved in another product: _"A tunnel's
encrypted transport packets route by `main`, never by a selection and never into a rejection.
Otherwise a selected tunnel carries its own transport, and a disconnected one can never reconnect."_
A satellite is that case permanently — its transport must ride the transit link via `main` while
everything else goes into the tunnel. Read that document before designing the satellite's ladder; the
same change also made empty gateway tables reject rather than fall through, which is a satellite's
steady state until its tunnel is up.

### 5. Subnet allocation, exhaustion and the existing guards.

`profiles × (1 + satellites)` `/24`s are needed, Core-allocated. `guard_subnet_collision`
(`profiles.rs:1271`) and `validate_profile_block` (`profiles.rs:1240`) see only local config and
enforce a single `/24`/`/16`. Open: the allocation scheme, the supported maximum, what happens when
the user's chosen LAN range cannot accommodate it, and how that error surfaces _before_ the admin
commits to a topology.

### 6. VLAN tag consistency across routers.

**Believed resolved (2026-09-21).** A satellite is paired blank and receives its tags from the Core
in the semantic payload, so there is nothing to reconcile — the question collapses into config sync.
Kept here in case a case is found where a satellite could hold tags of its own.

The design requires `vlan_tag` be globally identical. What enforces that when a satellite is paired
to a Core whose profiles already exist, and what happens if the satellite was previously paired
elsewhere?

### 7. Roaming: a device changes IP when it changes routers.

With per-router subnets, walking from the kitchen to the garage drops a device's address. Invisible
for most traffic; not for a long SSH session, a video call, a NAS mount or a self-hosted service
session. There is also no 802.11r/k/v fast-transition story. This is a real regression against a
consumer mesh, which does roam seamlessly. Open: whether this is acceptable and how it is
documented. Layer-2 extension would remove it entirely, and is one of the candidate mechanisms in
the attachment's cross-router service discovery concern.

### 8. Wi-Fi channel planning and co-channel interference.

Multiple routers on one SSID need channel coordination or they fight each other. Note that #3939 has
since landed regulatory-country support (`wifi.get`/`wifi.set` gain `country`, plus a new
`wifi.regulatory` reporting the channels an AP may currently use per band), which gives the Core the
data it would need to coordinate. Open: does the Core plan channels across satellites, or is it left
to the user — and if left to the user, what does the UI tell them? This extends #3466.

### 11. Clock and certificates on a WAN-less satellite.

A satellite cannot reach NTP until its tunnel is up. WireGuard tolerates clock skew; the satellite's
own HTTPS certificate, token expiry and log timestamps do not. What is the boot ordering, how does an
admin reach a satellite's local UI, does the Core's CA cover it, and is it reachable by name?

### 12. Pairing bootstrap is the new trust anchor.

Admin-initiated, single-use, short-lived enrollment code with key-fingerprint confirmation. This is
the moment a rogue device could become a trusted member of the network. Needs a dedicated review:
code entropy and lifetime, what the admin is asked to verify and whether they will actually verify
it, what happens to a half-completed pairing, and rate limiting.

### 13. The satellite's management RPC boundary.

The satellite's config-apply endpoint must be reachable **only** over the authenticated management
tunnel — never from the LAN or the underlay, where it would be a full remote-configuration surface on
an unauthenticated segment. Today `middleware/auth.rs` accepts a session cookie (`:98`), a local
cookie (`:108`) and any loopback peer (`:144`). #3670 plans to replace that stack entirely; satellite
auth should land as a middleware in the `start-core` OR-composition rather than as a fourth branch in
the current one.

### 14. The upstream channel is a new direction of trust.

The design starts as a pure Core→satellite push, but the device registry and the port-forward relay
require facts and requests flowing satellite→Core. The bounding invariant proposed is: _a satellite
may only report, or request anything about, addresses inside the subnets the Core allocated it._ That
must be enforced in code and reviewed alongside 11 and 12 — it was not in the original threat model.

### 17. IPv6 cannot be an afterthought, and its mechanism is unproven.

A profile must mean the same thing on every router; a satellite client getting IPv4-only while a Core
client on the same password gets dual-stack breaks the driving constraint.

- **DHCPv6-PD over a WireGuard interface has no precedent in this codebase.** A WG interface is a
  NOARP point-to-point device with no automatic link-local, while DHCPv6 solicits over link-local
  multicast to `ff02::1:2`. Spike this on two Linux boxes before locking the mechanism; no router
  hardware needed.
- **There is a working alternative in-house.** StartTunnel gives a _subnet_ a routed prefix
  (`WgSubnetConfig.ipv6: Option<Ipv6Net>`) and derives each host's `/128` from its tunnel IPv4
  (`tunnel/wg6.rs host_v6`), with explicit collision checks for prefixes smaller than a `/64`. That
  is the fallback — Core-central allocation pushed as delegated state — already written and tested.
  Note #3682 lists the _per-device_ derivation as "not porting"; the _subnet-level routed prefix_ is a
  different thing and is exactly the satellite case.
- **PD size is an external ceiling.** `profiles × (1 + satellites)` `/64`s: six profiles and two
  satellites is eighteen. A `/56` is comfortable, a `/60` is not enough, a `/64`-only ISP cannot do
  GUA at all. This compounds with #3862 — `wan_prefix` never reads the delegated size from netifd and
  defaults to a hardcoded `/48` (`lan.rs:339-355`) — so today the router cannot tell the user what
  their real ceiling is. Fix that first or satellites will be sized against a number the router made
  up.
- Whichever mechanism wins, hold the invariant: **prefixes arrive as delegated state over the
  authenticated tunnel with a lifetime, never as configuration in a semantic push.** An address is not
  meaning, and a prefix the ISP can renumber must not be frozen into a config push.
- `profiles.rs heal_ipv6_state` reconciles IPv6 _toward off_ and carries a note that it must be
  revisited if per-profile v6 states ever become legitimate. "Core on, satellite off" is exactly such
  a state.

### 18. The device registry is shared substrate, not the cost of one feature.

Four capabilities need satellite→Core device facts: the Devices page showing satellite clients, the
per-device automatic-forwarding toggle, port-forward authorization, and IPv6 published ports. Build it
once, deliberately. #4007 has since established the shape to follow: `devices.list` reports the UCI
static name as `custom_name` while the resolved name is only a placeholder, and `devices.update`
validates it as a hostname label. That is the fact/policy split the registry needs — a satellite
reports observations, the Core owns `custom_name`, reservations and toggles, and they travel down in
the semantic payload.

### 21. Firmware upgrades and version skew.

Core and satellites will not update atomically. What is the compatibility contract between a Core on
version N and a satellite on N-1? Does the Core refuse to sync to an incompatible satellite, how is
that surfaced, and is there a supported upgrade order?
