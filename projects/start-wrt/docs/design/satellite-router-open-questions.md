# Satellite Router — Questions Parked for Implementation

Working notes. **Not part of the issue package** (`satellite-router-issue-form.md` +
`SupportingEvidenceForSatelliteRouter.md`) — decisions to settle when the work is designed and
coded, recorded here so they are not lost between raising the issue and building it. §1 and §2 are
resolved and are carried into the design of record as D16 and D17; §3 is the surveyed
implementation surface, still open.

---

## 1. Backup, restore, and a satellite that is ahead of its Core

Raised 2026-09-21. **Resolved 2026-09-22.**

### What the code actually did

`backup.rs:57` runs `sysupgrade --create-backup`; `update.rs:300` runs plain `sysupgrade`. Both are
governed by the same set — OpenWrt's default (`/etc/config/*`) plus `/lib/upgrade/keep.d/startwrt`,
written by `build/stage-files.sh`. That list did not include `role.json` or `satellites.json`, so
three things were true before this was fixed:

- A satellite that took a firmware update **forgot it was a satellite**. `load_role()` defaults to
  Core, so it would reboot as a Core, run the Core-only block and start serving its own LAN.
- A Core that took a firmware update **forgot its satellites**, orphaning every pairing from the
  authoritative side.
- A Core's backup carried **no pairing material**, so a restore could not restore the network.

Both paths are now listed in keep.d. On shipped firmware the files do not exist, so the entries are
inert until this feature lands.

### One list, two purposes

keep.d means both "survives an upgrade" and "is included in a portable backup file," and there is no
way to say one without the other. A satellite's bearer token and WireGuard private key must survive
an upgrade; whether they belong inside a backup file someone may copy off the router is the #3662
question.

**Decision: put the material in the backup and rely on #3662 encrypting backups with the admin
password.** A backup that cannot restore the network is not a backup, and the alternative — omitting
pairing — forces physical access to every satellite on the day the Core has already failed. If
#3662's threat model later says bearer tokens must never travel, the answer is a second mechanism
(preserved-but-excluded), not dropping them from keep.d and losing them on upgrade.

### The generation collision

The scenario: a satellite is on generation 26, the Core's backup is from 25, the restored Core
pushes 25, and the satellite refuses it as stale.

**Decision: the Core catches up rather than the satellite resetting.** On reconnect the satellite
reports its applied generation; if the Core's is lower it raises its own counter above it and pushes
a full snapshot carrying its restored content. The satellite accepts it as newer and converges.

The Core stays authoritative and changes made after the backup are lost by design — the intended
outcome — but no satellite has to be factory-reset and re-paired to get there. The anti-rollback
property survives: it exists to stop a _replayed old_ snapshot from reinstating a deleted password,
and an attacker cannot mint a generation above the satellite's without the Core's credentials. An
attacker who has those is past this control anyway.

If the restored backup is already in step with its satellites, they simply reconnect, as after a
power cut.

### What must be visible

A silent convergence is the failure mode to avoid. Both ends should say that the Core was restored
from a backup older than the configuration the satellite was running, and that settings may have
changed. This is what makes the loss deliberate rather than mysterious.

### Satellites are not backed up

A satellite holds nothing authoritative, so a failed one is replaced, comes up blank and syncs.
Two things are genuinely lost on a reset and should be documented rather than discovered: its
**DHCP leases** (reservations are Core-owned and survive; dynamic leases do not, so devices behind
that satellite take new addresses) and its **local activity log**.

### Follow-on

When the feature ships, `docs/src/backups.md` ("What Is Included") needs a row for satellite
pairings, and `docs/src/updating.md` needs the multi-router ordering from §2.

---

## 2. Firmware upgrades across a Core and its satellites

Raised 2026-09-21. **Resolved 2026-09-22.**

The original proposal was blunt and security-first: upgrade the Core, let satellites drop off as
incompatible, flash each one, and re-pair from scratch. With `role.json` preserved (§1) the reset is
no longer necessary — a satellite keeps its role and its pairing across its own update and
reconnects.

### The deadlock in the strict version

A satellite has no WAN. It reaches the Internet only through the Core. If the Core is upgraded
first, finds the satellite incompatible, and that refusal **tears down the tunnels**, the satellite
loses egress — and can no longer download its own firmware. What should have been an in-app update
becomes a physical microSD reflash, on a box that may be in a garage.

### Decision

- **Upgrade the Core first.** It is the authority, and the ordering stays.
- **Version the sync payload, not the firmware.** Skew is then handled at exactly one interface
  instead of everywhere, which is what the original security argument was really asking for.
- **A version mismatch refuses to sync; it never drops the peer.** The transport — tunnel, routing,
  egress — stays compatible across versions. A satellite on an unsupported payload version keeps
  running its last applied configuration, keeps routing its clients, keeps reaching the Internet,
  and can therefore update itself.
- **The Core surfaces which satellites are behind**, and what that means: their configuration is
  frozen at the last generation they accepted until they are updated.

### Still open

Whether the Core should be able to push firmware to a satellite rather than each satellite fetching
its own, given that every byte travels through the Core regardless. That is a convenience decision,
not a correctness one, and it can wait for the update flow to be built.

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
