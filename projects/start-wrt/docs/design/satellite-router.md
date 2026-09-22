# Design: StartWRT Satellite Router Support

**Status:** Design of record. Decisions verified against the `start-wrt` backend and WireGuard
semantics. Implementation in progress on branch `start-wrt/satellite-router-v2`; filed upstream as
`Start9Labs/start-technologies#4043` on 2026-09-21. Where this document and the filed issue differ,
the issue and its attachment are the public statement and this document is being reconciled to them
— see §17.
**Scope:** `projects/start-wrt/` — extend a StartWRT network across one **Core** and one or more
**satellite** routers, keeping a single, centrally-controlled Security-Profile model. Inter-router
links are WireGuard tunnels (an implementation choice; see §2).

---

## 1. Problem Statement

StartWRT assigns every device a **Security Profile** (firewall zone + subnet + DHCP + VLAN +
outbound routing) by its **point of entry** — the Wi-Fi password it uses, or the Ethernet port it
plugs into. Today this works on a **single router**, so profiles cover only the area one router
reaches.

We want to extend coverage with one or more **satellite** routers while preserving the profile
model and keeping it transparent:

- The **same SSID and Wi-Fi password on any router** → the **same profile**, same Internet/LAN
  access, governed centrally.
- A device on a given **Ethernet port on any router** → the profile mapped to that port.
- **No client-side configuration** — just the password, or just plug in. The client never knows
  which physical router it used.

---

## 2. Architecture Overview

```
                            ┌───────────┐
                            │  Internet │
                            └─────┬─────┘
                                  │  WAN  (the only uplink)
                        ┌─────────┴─────────┐
                        │      CORE   C1     │   owns all profiles · source of truth
                        │  Guest 192.168.30.0/24   policy + egress enforced here
                        └───┬───────────┬───┘
        per-profile WG tunnels          per-profile WG tunnels
        (one per profile S1 serves)     (one per profile S2 serves)
                     ┌──────┴───┐    ┌───┴──────┐
                     │  SAT  S1  │    │  SAT  S2  │   no WAN · reaches Core via tunnels
                     │Guest .130.0/24│ │Guest .230.0/24│
                     └──┬─────┬──┘    └──┬─────┬──┘
                     Wi-Fi   eth      Wi-Fi   eth
                       │      │         │      │
                   [client][device] [client][device]

  Same "Guest" password (or Guest-mapped port) on C1/S1/S2 → the Guest profile
  everywhere, on three subnets (.30 / .130 / .230), all landing in the Core's
  Guest zone with one policy + one Internet egress. Each satellite carries each
  profile it serves over its own WireGuard tunnel to the Core.
```

**Roles (D6 — locked).** One **Core** (owns profiles, holds the only WAN, source of truth) + one or
more **Satellites** (followers, no WAN). Role is **chosen at setup and baked in at flash time;
changing role requires a reflash** — no runtime toggle. **Topology: hub-and-spoke** — each satellite
peers directly with the Core, no chaining.

**Points of entry on a satellite (resolved locally, then carried to the Core).**

- **Wi-Fi** — every router broadcasts the same SSID + password set; the router the client associates
  to maps password → profile VLAN locally via per-PSK dynamic VLAN. Transparent to the client.
- **Ethernet** — each satellite LAN port maps to a profile (per-port, not per-device), exactly like
  the Core. One port is the **uplink** carrying the tunnels to the Core.

**Transport — per-profile WireGuard tunnels (D2 — locked, Option A).** For each profile a satellite
serves, one WireGuard tunnel carries that profile's traffic to the Core. **Tunnels are
router-to-router, never per-device** — the count is _profiles × satellites_ (e.g. 6 profiles × 2
satellites = 12; ~6 per satellite), independent of how many clients connect. WireGuard tunnels are
near-free and are created/managed invisibly by pairing (the admin sees "S1 is paired"). Each tunnel
joins its profile's firewall zone at the Core, so **all** existing per-profile firewall/routing/
policy applies with essentially no new firewall logic. A small management/RPC channel (the D4 trust
anchor) rides a dedicated tunnel.

**Addressing — routed attachment, per-router subnets (D1 — locked).** The Core keeps single-`/24`
profiles. Each satellite owns its **own `/24` per profile**, advertised over that profile's tunnel
and **attached** to the Core's matching profile as a routed subnet (its tunnel interface joins the
profile's `vlan_<iface>` zone; its `/24` is routed into the profile's per-VLAN table). Same profile
_policy_, different subnets.

**Policy & egress — enforced at the Core.** The Core is authoritative for what a profile _means_
(firewall zone, LAN/WAN access, DNS, outbound routing / VPN chain); Internet egress happens at the
Core. Satellites keep profiles isolated locally and route everything else up the tunnels.

**Configuration — Core-authoritative semantic push (D5).** The Core pushes a small **semantic**
payload — `{ profiles:[{vlan_tag, wan/lan/dns/outbound policy}], passwords:[{key,vid,label}],
ports:[{port,vlan_tag}], ssid, admin_key }` — and each satellite **regenerates its own**
subnet/DHCP/zone/routing locally by running the existing `profiles.rs` rewrite chain against its own
`/24`s. Pushing the _meaning_ (not raw UCI or a full backup) avoids clobbering satellite-local
identity (hostname, LAN IP, certs, admin password, WG keys, role). Satellites never author profile
state; they reconcile on reconnect after any missed edit.

---

## 3. Design Decisions (resolved)

| #   | Decision                                     | Resolution                                                                                                                                                                                                                                                                                                                               | Status                                     |
| --- | -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------ |
| D1  | Profile model for multi-router subnets       | **Routed attachment** — single-`/24` profiles + attach remote subnets to the zone/table                                                                                                                                                                                                                                                  | ✅ locked                                  |
| D2  | Tunnel topology / profile separation         | **Option A — per-profile L3 tunnels** (~profiles×satellites; full zone reuse)                                                                                                                                                                                                                                                            | ✅ locked                                  |
| D6  | Role selection & mutability                  | **Chosen at initial setup; changed only by erasing role state, resetting to factory defaults and rebooting into the new role** — never switched in place (revised 2026-09-21)                                                                                                                                                            | ✅ locked                                  |
| D3  | Satellite Internet egress (no WAN)           | Reuse per-profile policy routing, with explicit **WAN-less** handling (endpoint via local link, DNS re-pointed, kill-switch neutralized)                                                                                                                                                                                                 | recommended                                |
| D4  | Pairing & remote-peer auth                   | Enrollment code → persistent per-pairing token, backed by `ed25519` identity, **bound to the management-tunnel source**; new auth path in `middleware/auth.rs`. **Needs security review.**                                                                                                                                               | recommended                                |
| D5  | Config sync                                  | **Push a semantic payload**; satellite regenerates locally; reconcile on reconnect                                                                                                                                                                                                                                                       | recommended                                |
| D7  | Role capability gating                       | **Middleware** gate keyed on role (allowlist of satellite-local endpoints)                                                                                                                                                                                                                                                               | recommended                                |
| D8  | Subnet/VLAN coordination                     | `vlan_tag` **globally identical**; **Core-central** `/24` allocation per satellite; teach the two subnet guards about satellite subnets                                                                                                                                                                                                  | recommended                                |
| D9  | DNS & DHCP split                             | **Satellite-local** DHCP and DNS per profile (mirrors the current per-gateway model)                                                                                                                                                                                                                                                     | recommended                                |
| D10 | Packaging / image                            | WireGuard already ships; **no RADIUS**; verify `wireguard-tools`/`kmod-wireguard` only                                                                                                                                                                                                                                                   | recommended                                |
| D11 | Satellite IPv6 addressing                    | **Prefixes arrive as delegated state over the authenticated tunnel, never in the semantic payload** (§13). Mechanism: DHCPv6-PD on the management tunnel                                                                                                                                                                                 | invariant ✅ locked; mechanism recommended |
| D12 | Automatic port forwarding behind a satellite | **Satellite relays, Core authorizes** (§15); absent from v1 behind an explicit refusal                                                                                                                                                                                                                                                   | ✅ locked                                  |
| D13 | Satellite device registry                    | **Satellite reports device facts; Core owns per-device policy** (§14) — shared substrate for D11, D12, and the device UI                                                                                                                                                                                                                 | requirement ✅ locked; shape recommended   |
| D14 | Cross-router service discovery               | mDNS does not cross a routed boundary, so one profile stops being one discovery domain. Three candidates: layer-2 extension (VXLAN/GRETAP inside the tunnel), a per-profile mDNS reflector, or DNS injection as `start-core` already does for tunnel clients. **Open** — the problem is certain, the mechanism is not (added 2026-09-21) | ⬜ open                                    |
| D15 | Target scale                                 | Benchmark configuration is **one Core, two satellites, fifteen profiles**. Today's one-LAN-port hardware caps a real deployment at one satellite (added 2026-09-21)                                                                                                                                                                      | ✅ locked                                  |

---

## 4. End-to-end walkthrough (a Guest client on satellite S1)

1. Client joins the shared SSID with the **Guest** password on **S1**. S1's local hostapd (per-PSK
   dynamic VLAN, no RADIUS) puts it on S1's Guest VLAN → S1's Guest `/24` (`192.168.130.0/24`), DHCP
   from S1.
2. Client's traffic egresses S1's Guest interface into **S1's Guest→Core WireGuard tunnel**
   (`AllowedIPs` routes it there).
3. At the Core the tunnel interface is a member of the **Guest zone**, so the packet is treated
   exactly like a local Guest client: Guest firewall policy (WAN/LAN access), Guest outbound routing
   (direct or a VPN chain), Guest DNS.
4. Internet-bound traffic exits the **Core's WAN** (the only uplink); LAN-bound traffic reaches
   Core-side Guest-permitted resources. Return traffic follows the tunnel back (the client `/24` is
   in the peer's `AllowedIPs` and routed via the tunnel).
5. The same client on **C1** or **S2** with the same password gets the identical Guest policy, on
   `.30` / `.230` respectively. Transparent throughout.

---

## 5. Verification & workability (checked against the code + WireGuard semantics)

- **Zone reuse for a routed subnet — CONFIRMED.** `FirewallZone.network` is a `Vec<String>` and zone
  membership/forwarding is keyed by **ingress interface name**, not subnet (`profiles.rs`
  `rewrite_firewall`, `ensure_firewall_zone`). Adding a per-profile tunnel to the profile's zone
  makes the satellite's downstream `/24` inherit WAN access, LAN access, cross-profile forwarding,
  and egress with no per-subnet special-casing. This is the linchpin of Option A and it holds.
- **WireGuard cryptokey routing — CONFIRMED applicable.** `allowed_ips` is dual-purpose (outbound
  route selection + inbound source ACL); a peer may carry a `/24` (or `0.0.0.0/0`), and
  `route_allowed_ips=1` auto-installs the route. So the Core's satellite-peer lists the satellite's
  Guest `/24`; the satellite's Core-peer lists the Core-reachable subnets + `0.0.0.0/0` for egress.
  Each WG interface binds **one** listen port (hence Option A's per-profile interfaces each need a
  port — see risks).
- **Per-VLAN policy table generalizes — CONFIRMED.** For VPN-routed profiles the remote `/24` needs
  a source ip-rule (`src <remote/24> lookup <vlan_tag>`) + a table route — the existing
  `prr_`/`plr_` machinery is prefix-agnostic (`profiles.rs` `rewrite_routing`; `NetworkRoute.target`
  is a free string already used with `/24`s).
- **Config regeneration payload — CONFIRMED minimal.** The only cross-router essentials are
  `{ vlan_tag, passwords(key+vid+label), ports }` per profile + SSID/admin-key; the satellite
  manufactures all subnet/DHCP/zone/routing itself via the existing rewrite chain.

---

## 6. Risks & gotchas identified in advance (with mitigations)

1. **WAN-less satellite egress & endpoint pinning — TOP RISK.** `rewrite_vpn_chain_routes`
   (`vpn_client.rs:1220`) pins a VPN endpoint's `/32` via the target interface assuming a base uplink
   exists; the per-profile policy tables and the kill-switch `unreachable` fallbacks
   (`profiles.rs rewrite_routing`) also assume a WAN default. A satellite has no WAN. _Mitigation:_
   pin each Core tunnel-endpoint `/32` via the **local transit link** gateway; re-point DNS
   (`peerdns=0`) at the Core/local resolver; neutralize the WAN kill-switch semantics on satellites.
   **Must be validated on hardware** — this is where the most new logic lives.
2. **MSS/MTU clamp on tunnel ingress — none today.** The inbound `wg_<P>` writes `mtu:None` and the
   profile zone has no `mtu_fix`, so large TCP from satellite hosts would black-hole. _Mitigation:_
   set a correct tunnel MTU and/or add `mtu_fix` on the tunnel/zone (reuse the pattern in
   `ensure_vpn_outbound_zone`).
3. **Remote-peer auth is a brand-new attack surface.** Today `middleware/auth.rs` accepts only
   loopback / local cookie / admin session. _Mitigation:_ a per-pairing token bound to the
   management-tunnel source IP, `ed25519`-backed; **dedicated security review** before ship.
4. **Listen-port allocation.** Ports are user-set with a uniqueness check today; Option A needs ~6
   auto-allocated UDP ports per satellite, and the accept rule's `src` is hardcoded `"wan"`
   (`vpn_server.rs:2001`). _Mitigation:_ pairing-time free-port allocator; parameterize the accept
   rule's source zone to the transit-link zone.
5. **proxy-ARP is wrong for a routed subnet.** `sync_proxy_arp` assumes on-link host peers in the
   profile `/24`. _Mitigation:_ skip proxy-ARP for satellite subnets — they're reached by route.
6. **Subnet guards are local-only.** `guard_subnet_collision` / `validate_profile_block` only see
   local config and enforce one `/24`/`/16`. _Mitigation:_ Core-central allocation that records and
   validates satellite-assigned subnets.
7. **Offline reconcile / eventual consistency.** A satellite that missed edits must converge on
   reconnect. _Mitigation:_ satellite pulls a full semantic snapshot on (re)connect, not just deltas.
8. **Host-scoped peer model in `vpn_server.rs`.** IP allocation, `/32` `AllowedIPs`, per-octet route
   naming, and the config generator all assume one host in the profile `/24`. _Mitigation:_ a **new
   `vpn_site.rs`** module + `vpn_site` UCI type for subnet peers, reusing only the crypto/interface/
   firewall-rule primitives — don't overload the road-warrior path.
9. **DHCPv6-PD over a WireGuard interface is unproven here — GATES D11's mechanism.** Nothing in
   this codebase has ever run RA or DHCPv6 over a `wg_*` interface (`vpn_server.rs` / `vpn_client.rs`
   set no `ra`/`dhcpv6`/`ip6assign`), and a WG interface is a NOARP point-to-point device with no
   automatic link-local, while DHCPv6 solicits over link-local multicast to `ff02::1:2`.
   _Mitigation:_ **a bench spike before D11's mechanism is locked** — two WG peers, `odhcpd`
   delegating on one, a DHCPv6 client requesting a prefix on the other, `ff02::1:2` inside
   `allowed_ips`. Needs no satellite and no K1, just two Linux boxes. If it fails, fall back to
   Core-central `/64` allocation pushed as delegated state (§13). **Must be validated before the v6
   phase starts.**
10. **IPv6 PD size is an external ceiling.** `profiles × (1 + satellites)` `/64`s are needed; a
    `/60` ISP delegation cannot cover a modest deployment and a `/64`-only one cannot do GUA at all.
    _Mitigation:_ none available to us — inherit the ULA→GUA/NAT66 work and document the PD-size
    requirement as a satellite prerequisite (§13).
11. **The upstream channel is a new direction of trust.** D5 made the satellite a pure follower;
    D12 and D13 require device facts and forward requests flowing satellite→Core, which §12's threat
    table was not written against. _Mitigation:_ the bounding invariant — a satellite may only report
    or affect addresses inside the subnets the Core allocated it (D8) — plus threat #11 below.

---

## 7. Impact — file-by-file change map (magnitude)

| Area / file                                                                                                               | Change                                                                                                                                                                    | Magnitude          |
| ------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------ |
| `backend/ctrl/src/vpn_site.rs` **(new)**                                                                                  | Site-to-site WG: subnet-advertising peer, transit-underlay addressing, per-profile tunnel bring-up, skip proxy-ARP, prefix `AllowedIPs`/routes, WAN-less endpoint pinning | **Large (new)**    |
| `backend/ctrl/src/satellite.rs` **(new)**                                                                                 | Role state, pairing/enroll, semantic config-sync RPC (`pair`/`enroll`/`sync`/`list`/`unpair`), satellite registry                                                         | **Large (new)**    |
| `backend/uciedit/src/openwrt.rs`                                                                                          | New typed UCI sections (`vpn_site`, role marker, satellite registry)                                                                                                      | Medium             |
| `backend/ctrl/src/profiles.rs`                                                                                            | Routed attachment: remote-subnet source-rule + table route + zone membership; enumerate remote subnets in cross-routes; guards learn satellite subnets; MSS on ingress    | Medium             |
| `backend/ctrl/src/vpn_server.rs`                                                                                          | Parameterize `ensure_wireguard_firewall_rule` source zone; factor reusable WG helpers for `vpn_site`                                                                      | Small–Medium       |
| `backend/ctrl/src/middleware/auth.rs` + `auth.rs`                                                                         | New remote-peer auth path / per-pairing token bound to tunnel source                                                                                                      | Medium–Large       |
| `backend/ctrl/src/bins/daemon.rs`                                                                                         | Role gate around the Core-only normal-mode block in `inner_main`                                                                                                          | Medium             |
| `backend/ctrl/src/setup.rs`                                                                                               | Role choice at flash + `SetupStatusRes`                                                                                                                                   | Medium             |
| `backend/ctrl/src/wifi.rs`, `ethernet.rs`                                                                                 | Emit the semantic sync payload; satellite-side regenerate                                                                                                                 | Small–Medium       |
| Role capability gating (cross-cutting)                                                                                    | Middleware allowlist keyed on role                                                                                                                                        | Medium             |
| `backend/ctrl/src/lib.rs`                                                                                                 | Register `satellite` (+ `vpn_site`) module; role on context                                                                                                               | Small              |
| Config-sync transport                                                                                                     | Reuse `CliContext::call_remote` / `call_registry_rpc` over the management tunnel                                                                                          | Medium             |
| `API_CONTRACT.md`                                                                                                         | Document the `satellite.*` module                                                                                                                                         | Medium             |
| `web/` — `app.routes.ts` / `routes/settings` + new `routes/satellites`, `api.service.ts` + `live-api` + `mock-api`        | New "Satellites" surface (list, pair dialog, status); pattern-match `published-ports`                                                                                     | **Large (new UI)** |
| `build/openwrt.diffconfig`                                                                                                | Verify WireGuard packages present                                                                                                                                         | Small (verify)     |
| **IPv6 (D11)** — `vpn_site.rs`, `profiles.rs`, satellite `reqprefix` on the mgmt tunnel                                   | Prefix delegation over the tunnel; interface-keyed `rule6` attachment; revisit `heal_ipv6_state`                                                                          | **Large (new)**    |
| **Device registry (D13)** — new upstream sync direction + `devices.rs` / `device_ident.rs` / `ipv6_tracker.rs` read paths | Satellite reports device facts; Core renders them and owns policy                                                                                                         | **Large (new)**    |
| **Port-forward relay (D12)** — satellite-side `port_control.rs` listener + Core-side authorize path                       | Terminate PCP/UPnP locally, relay upward; replace `arrival_matches` with the tunnel+subnet check                                                                          | **Large (new)**    |
| v1 refusal path (D12) — satellite `port_control.rs` + `web/`                                                              | Explicit PCP error / UPnP fault + UI note that the feature is unavailable behind a satellite                                                                              | Small              |

---

## 8. Suggested phasing (each independently testable)

1. **Role + provisioning** (D6) — role marker, wizard choice, `daemon.rs` gate; satellite boots
   "empty" without Core behaviors.
2. **Site-to-site transport** (D2/D10) — `vpn_site.rs`: a per-profile tunnel with a routed `/24`,
   **WAN-less endpoint** handling (D3) — the top-risk item, validate on hardware early.
3. **Core routing/firewall attachment** (D1) — land a satellite `/24` in its profile zone/table;
   verify a manually-placed satellite host gets full profile policy + Internet via the Core.
4. **Pairing + remote auth** (D4) — `satellite.rs` enroll → token; `middleware/auth.rs` path
   (security review here).
5. **Config sync** (D5) — semantic push + satellite regenerate; verify same-password-same-profile
   across routers end-to-end.
6. **Capability gating** (D7) + **UI** (satellites page, pairing flow).
7. **v1 refusal path** (D12) — explicit PCP/UPnP refusal + UI note. Small, and it belongs _in_ v1:
   without it the failure is silent.
8. **Device registry** (D13) — satellite reports device facts; Core renders them in the device list.
   Unblocks the per-device toggle, D12's authorization, and v6 published ports at once.
9. **IPv6** (D11) — run the risk #9 spike first, then prefix delegation + the interface-keyed
   `rule6` attachment. Reuses phase 3's zone/table plumbing.
10. **Automatic port forwarding** (D12) — satellite-side listener relaying to the Core authorizer.
    Requires phase 8.

---

## 9. Out of scope / non-goals

- **Per-device authentication on a shared wired port** — assignment is per-port; any device (or dumb
  switch) on a port gets that port's profile.
- **Same-subnet seamless roaming** between routers — a device changing routers changes IP. (Would
  require L2-over-WireGuard, considered and set aside.)
- **Daisy-chaining satellites** — hub-and-spoke only.
- **Runtime role changes** — role is fixed at flash.
- **Wi-Fi backhaul** — v1 backhaul is cable/LAN; wireless backhaul is a documented **future
  enhancement** (§11), security-neutral but deferred for performance/bootstrap reasons.
- **IPv6 on satellites in v1** — v1 serves IPv4 only. _Deferred, not excluded_: v6 is a requirement
  (§13, D11) and the v1 data shapes already carry it without a schema migration.
- **Automatic port forwarding on satellites in v1** — deferred (§15, D12), and v1 must **refuse it
  explicitly** rather than fail silently. Required for full release, not for a proof of concept.

---

## 10. Tunnel & credential lifecycle

**Tunnel creation — eager, at sync time.** A satellite rebroadcasts the shared SSID with **all**
profile passwords, so any profile could be used at any moment; therefore the satellite establishes a
tunnel for **every profile it can serve** (each profile that has a Wi-Fi password, plus any profile
mapped to one of its Ethernet ports) as soon as that profile is synced. Consequently **creating a
new profile at the Core → the next config-sync push → the satellite brings up the tunnel
immediately** (latency = sync propagation, seconds — _not_ deferred to a client's first use, so
there is no first-connection delay).

**Why not lazy / on-first-use.** WireGuard has no native on-demand bring-up; a lazy scheme needs a
client-presence trigger, adds first-connection latency, and creates a teardown race (a client
arriving mid-teardown is stranded). Idle WG tunnels are near-free — a netdev + a UDP port, silent
when idle (or one small `persistent_keepalive=25` packet). The savings don't justify the complexity.

**Tunnel teardown — deterministic events only, never inactivity.** A tunnel is removed when its
profile is **deleted**, when the satellite **stops serving** that profile (its password is removed
and no port maps to it), or when the satellite is **unpaired**. **No idle-timeout teardown** — the
race/latency/churn cost outweighs reclaiming a near-free idle interface. (Keepalive can be dropped
for a cable/LAN backhaul with no NAT; keep it for Wi-Fi/NAT paths.)

**Credential / config sync — when & how often.**

- **At pairing** — the satellite pulls a full semantic snapshot.
- **On every profile/password/port edit** at the Core — an event-driven **push** (steady state = no
  traffic; a change = one push).
- **On reconnect** — a full-snapshot reconcile to catch anything missed while offline.
- **Periodic version heartbeat** — compare a config **generation number** every few minutes as a
  drift safety-net.

**Versioning / anti-rollback.** Each snapshot carries a **monotonic generation number**; a satellite
refuses to apply anything older than its current (blocks rollback), and the Core records each
satellite's applied generation so the UI can show up-to-date vs. stale satellites.

**Staleness risk — bounded.** The realistic exploit is a **revoked/changed password still accepted
at an offline satellite** until it re-syncs. It is bounded: the attacker must be in Wi-Fi range of
_that specific_ stale satellite, the window ends at reconnect (usually seconds/minutes), and there is
**no privilege escalation** — a stale credential grants the _same_ profile it always did, never more.
Mitigations: reconnect-reconcile, generation numbers, and a UI indicator when a satellite hasn't
acked a security-relevant change. (Instant network-wide revocation is a RADIUS property deliberately
traded away for the transparent-password UX — see D9.)

---

## 11. Backhaul medium (cable / LAN / Wi-Fi)

**The medium is a transport choice, not a security boundary.** All authentication and encryption live
in the WireGuard tunnel; the underlay is untrusted by design. The satellite therefore needs **no
particular port** and derives **no trust from the port or a Wi-Fi password** — exactly as intended.
Any path giving IP reachability to the Core's WG endpoint works.

- **Direct cable (recommended default)** — dedicated point-to-point link; best bandwidth/latency and
  most reliable. Because the Core is the only WAN, a robust backhaul matters.
- **Existing LAN** — the satellite plugged into any port/switch that can route to the Core endpoint.
- **Wi-Fi backhaul — FUTURE ENHANCEMENT (not in v1).** The satellite could instead associate as a
  Wi-Fi _station_ for underlay connectivity, then tunnel. Security would be unchanged (the medium is
  not a trust boundary — the WireGuard tunnel is), so this is a _viable_ future option; it is deferred
  from v1 for **performance/reliability** reasons and an unresolved bootstrap sub-decision. Notes for
  when it is picked up: this hardware has **two radios** (2.4 GHz + 5 GHz), so one band could be
  dedicated to backhaul and the other to client service, avoiding the single-radio repeater
  throughput-halving (at the cost of that band for clients); single-radio backhaul roughly halves
  throughput and adds latency; Wi-Fi is less robust than cable, and since the satellite's entire
  uplink (Internet included) is the tunnel, a flaky backhaul degrades everything; and a bootstrap
  decision remains — which credential the satellite uses to _associate_ for the underlay (a dedicated
  infrastructure association vs. reusing an existing one).

**Core-side implication.** The Core must accept the WG handshake on whichever interface the satellite
arrives on (cable/LAN today; Wi-Fi later), not just WAN (the accept-rule source-zone parameterization
already in the risk list). Exposing a WG listen port on the LAN is low-risk: **WireGuard is silent to
unauthenticated packets** — no response without a valid handshake, hence no open-port fingerprint or
unauthenticated attack surface.

---

## 12. Security review (home / small-business threat model)

**Framing.** Target is home + small business. The bar is **no obvious holes** and a model at least as
strong as a good prosumer mesh — explicitly **not** military/intelligence resistance. Accepted
residual risks are called out with why they are proportionate here. Overall the inter-router link is
**WireGuard end-to-end, stronger than typical consumer mesh backhaul**; the two genuinely new trust
surfaces are the **pairing bootstrap** and **credential replication to more devices**, both
adequately mitigated.

| #   | Threat                                                                                                                          | Mitigation                                                                                                                                                                           | Residual severity (home/SMB)                                                                                                                                                                       |
| --- | ------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Link tap / splice / MITM (cable or Wi-Fi backhaul)                                                                              | WG encrypt+authenticate; pinned static keys prevent MITM; can't read or inject                                                                                                       | **None** — stronger than a plain LAN cable / VLAN trunk                                                                                                                                            |
| 2   | **Rogue satellite** (impersonate to gain profile access / inject config)                                                        | Pairing needs WG keypair + **single-use, short-lived, admin-initiated** enrollment code; Core trusts only paired satellites (pubkey + token bound to tunnel source)                  | **Moderate** — the key bootstrap moment; top security-review item                                                                                                                                  |
| 3   | Rogue Core (push malicious config)                                                                                              | Satellite pins Core key/identity at pairing; only accepts the authenticated Core afterward                                                                                           | Low post-pairing (verify code/fingerprint at pairing)                                                                                                                                              |
| 4   | Sync injection / rollback                                                                                                       | Rides the authenticated+encrypted tunnel; monotonic generation numbers reject old configs                                                                                            | Low                                                                                                                                                                                                |
| 5   | **Stale credential after revocation**                                                                                           | Reconnect-reconcile + generation numbers + staleness UI                                                                                                                              | **Low–Moderate; accepted** — bounded window, needs physical proximity, **no privilege escalation**; enterprises needing instant revocation use RADIUS (which we skip for transparency)             |
| 6   | **Physical theft of a satellite** (exposes plaintext PSKs, WG keys, token)                                                      | **One-click unpair revokes it at the Core instantly**; guidance to **rotate Wi-Fi passwords**; RPC firewalled to the tunnel                                                          | **Moderate; accepted** — same posture as today's single router (plaintext PSKs are unavoidable for the transparent-password model), extended to more devices; we don't target tamper-resistant APs |
| 7   | **Satellite management-RPC exposure**                                                                                           | Config-apply endpoint authorized **only over the authenticated tunnel** (token bound to source; firewall RPC to the tunnel), never from the LAN/Wi-Fi underlay                       | **Moderate** — a must-get-right; part of the D4 auth design                                                                                                                                        |
| 8   | Cross-profile isolation over the tunnel                                                                                         | Reused zone model (satellite separates profiles by VLAN; Core enforces cross-router forwarding)                                                                                      | Standard — mitigate with explicit isolation tests                                                                                                                                                  |
| 9   | Availability (Core down → satellite island)                                                                                     | Inherent to Core-only-WAN                                                                                                                                                            | Availability property, not a breach                                                                                                                                                                |
| 10  | WG listen port on LAN/Wi-Fi                                                                                                     | WG silent to unauthenticated packets                                                                                                                                                 | Negligible                                                                                                                                                                                         |
| 11  | **Upstream channel abuse** (a paired satellite reporting bogus devices, or requesting forwards for addresses it does not serve) | Core authorizes, never the satellite (D12); every report and request bounded to subnets the Core allocated that satellite (D8/D13); arrival on satellite X's tunnel proves X sent it | **Moderate** — new trust direction; review alongside #2 and #7                                                                                                                                     |

**Bottom line.** Appropriate for the intended home/SMB use. The two items warranting focused security
review are the **enrollment bootstrap (#2)** and the **satellite RPC authorization boundary (#7)**.
The accepted residual risks — **eventual-consistency revocation (#5)** and **physical-theft credential
exposure (#6)** — are inherent to the transparent single-password model, matched by unpair + rotate +
versioning, and proportionate for this market; neither is an _obvious_ hole, and both are documented
so an operator understands the trade.

---

## 13. IPv6 addressing (D11)

**Status:** the invariant is **locked**; the mechanism is **recommended**, pending a bench spike
(risk #9).

Satellite profiles are **dual-stack**. A profile means the same thing on every router (§1), so a
satellite client that gets IPv4-only service while a Core client using the same password gets
dual-stack breaks the design's driving constraint. v1 ships IPv4-only (§9) — designed for v6, not
excluding it.

**The invariant (locked).** IPv6 prefixes reach a satellite as **delegated state carried over the
authenticated tunnel, with a lifetime** — never as semantic-payload configuration. D5's payload
stays _meaning_ (`vlan_tag`, passwords, ports); an address is not meaning, and a prefix an ISP can
renumber out from under us must not be frozen into a config push. The delegation rides the
**management tunnel**, not the raw transit link: §11 makes the underlay untrusted by design, so
addressing taken from the bare link would arrive unauthenticated.

**Why the v6 attachment is simpler than the v4 one.** The v4 routed attachment (D1) needs a source
ip-rule (`src <remote/24> lookup <vlan_tag>`) plus a table route. The v6 path needs neither, because
it was already built prefix-agnostic: `profiles.rs rewrite_routing` installs `prl6_<iface>` /
`prr6_<iface>` keyed on **ingress interface**, explicitly because "LAN `/64`s are dynamic under
DHCPv6-PD". A satellite's per-profile tunnel is an interface in the profile's zone, so v6 policy
routing attaches with an interface-named `rule6` and nothing prefix-shaped.

> **Design rule.** The v6 attachment is **interface-keyed**; the v4 attachment is **prefix-keyed**.
> Don't build the v4 source-rule machinery as though prefix matching were the only attachment
> mechanism, or v6 will look like a special case when it is in fact the simpler one.

**What does not work today.** A satellite has no WAN, therefore no delegated prefix, therefore no
pool for `ip6assign`. Under D5 the satellite regenerates locally through the same `profiles.rs`
chain, and that chain writes `ip6assign 64` on every profile interface whenever IPv6 is on — against
an empty pool on a satellite. The failure is silent: satellite clients simply have no v6 while Core
clients do.

**Mechanism (recommended).** The satellite requests a prefix on the management tunnel (`reqprefix`,
the same `NetworkInterface` field the WAN already uses) and the Core's `odhcpd` delegates from its
own PD. The satellite's existing per-profile `ip6assign 64` then works **unchanged** — netifd carves
`/64`s exactly as it does on the Core — and an ISP renumber propagates by protocol instead of
through our sync. In v6 terms the management tunnel simply _is_ the satellite's uplink.

_Fallback if the spike fails:_ the Core allocates `/64`s centrally (mirroring D8's `/24` allocation)
and pushes them over the management RPC as delegated state carrying a lifetime. Same invariant,
hand-rolled renumbering.

**PD size is the ceiling, and it is the ISP's, not ours.** Each profile consumes a `/64`, so
satellites scale demand from `profiles` to `profiles × (1 + satellites)` — 6 profiles and 2
satellites is 18. A `/56` is comfortable, a `/60` (16) is not enough, and a `/64`-only ISP cannot do
GUA at all. This ceiling **already exists for profiles today**: see `profiles.rs`
`TODO(ipv6/nat66)` — on a `/64`-only ISP the single GUA `/64` goes to the admin LAN and every other
profile gets a ULA with no v6 internet path. The satellite design does **not** solve NAT66; it
inherits whatever the ULA→GUA redesign lands. Document the PD-size requirement as a satellite
prerequisite.

**Revisit list.** `profiles.rs heal_ipv6_state` reconciles v6 _toward off_ and carries a NOTE that it
must be revisited if per-profile v6 states ever become legitimate. "Core v6 on, satellite v6 off" is
exactly such a state, so that heal must be revisited when satellite v6 lands.

---

## 14. Satellite device registry (D13)

**Status:** the requirement is **locked**; the shape is **recommended**.

**The design is unidirectional today.** D5 is a Core→satellite push, and a satellite "never authors
profile state". But four separate capabilities all need the opposite direction — device _facts_
flowing satellite→Core:

| Capability                                    | Why it needs the registry                                                                                 |
| --------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| The Devices page showing satellite clients    | the Core enumerates from `ip neigh show` plus the dnsmasq lease files (`devices.rs`), both strictly local |
| The "Allow automatic port forwarding" toggle  | it is a per-device control on the device detail page, and a satellite device has no page                  |
| Automatic port forwarding authorization (§15) | `port_control.rs resolve_client` needs a MAC and an interface it can trust                                |
| IPv6 published ports                          | `ipv6_tracker` elects a device's stable GUA from `ip -6 monitor neigh`, Core-local                        |

The registry is therefore **not a cost of D12 alone** — it is shared substrate under D11, D12, and
the device UI. That is the argument for building it once, deliberately, rather than three times by
accident.

**Shape (recommended).** A satellite reports, per device it serves: MAC, current addresses (v4 and
v6), the profile it is on, hostname / DHCP fingerprint, and last-seen. The Core stores these against
the reporting satellite and renders them in the device list, marked with which satellite they are
behind. Per-device **settings** — name, the PCP toggle, reservations — remain **Core-authoritative**
and travel _down_ in the D5 payload. The satellite reports facts; the Core owns policy. That keeps
D5 intact: a report is an observation, not a configuration.

**Bounding invariant.** A satellite may only report — and may only affect — addresses inside the
subnets the Core allocated to it. D8 already makes the Core the allocator, so this is checkable
without trusting the satellite. A compromised satellite can then misbehave only toward its own
clients, which it can do anyway by virtue of being their gateway.

**Eventual consistency.** The registry is exactly as stale as §10's credential replication, and
bounded the same way: reconcile on reconnect, generation numbers, and a UI indicator for a satellite
that has not acked. Note that `device_ident.rs` derives OS and vendor from the DHCP exchange — which
under D9 happens **on the satellite** — so a satellite identifies its own clients locally and ships
the result up, rather than the Core guessing about a device it cannot see.

---

## 15. Automatic port forwarding behind a satellite (D12)

**Status:** **locked** — the satellite relays, the Core authorizes. Deferred out of v1 (§9) behind an
explicit refusal.

**PCP and UPnP are link-scoped by design.** A PCP client sends to its own default gateway; UPnP is
discovered by SSDP multicast on the local segment. A device on S1 sends both to **S1**, never to the
Core, whatever we build — so a satellite must terminate these protocols locally. The only real
question is where authorization lives.

**D12 (locked): the satellite relays, the Core authorizes.** The satellite terminates the protocol
and forwards the request, plus device identity, up the management tunnel; the Core decides using the
§14 registry and owns the resulting forward. The Core stays the single arbiter of external ports —
which it must be, since it already refuses ports the router itself answers on and resolves
manual-rule overlaps. The alternative (satellite authorizes, Core installs) is rejected: it would let
a compromised satellite authorize a device the admin never permitted, contradicting D5.

**A security check is being replaced, not reused.** The Core's cross-segment spoof defence cannot
apply to a routed client: `port_control.rs arrival_matches` requires the arrival ifindex to equal the
interface the neighbor table places the claimed source on, and a satellite client appears in neither.
The sound substitute is two facts taken together — **arrival on satellite X's tunnel proves satellite
X sent it** (the tunnel is authenticated), **and the claimed device must live in a subnet the Core
allocated to X** (§14). Flag this explicitly in the D4 security review; it is a replacement, not an
inheritance.

**Link-drop policy.** Lease bookkeeping is deliberately in-memory at the Core, and clients renew
every few minutes. When a satellite's tunnel drops, renewals stop. Forwards should **survive the drop
and expire on the normal sweep**: the device is unreachable anyway, so a stale forward costs nothing,
while dropping one immediately breaks a device that was only briefly disconnected. The sweep's
existing address-binding rule still applies — a forward whose owning MAC no longer holds the address
it points at is collected regardless.

**SNI hostname routes** ride the same path once identity is solved; routing a hostname to a satellite
device is a DNAT to a routed address. One documented wrinkle compounds: a _local_ client reaching a
routed hostname already appears in the device's logs as the router's own address, and behind a
satellite that is a second hop of address rewriting.

**v1 behavior (locked): explicit refusal.** A v1 satellite does not support automatic port
forwarding, and must **say so** — a PCP error response and a UPnP fault indicating the feature is
unavailable behind a satellite, plus a visible note in the UI — never silence. Left alone the request
dies at the Core as `NOT_AUTHORIZED` behind a `tracing::debug!`: a StartOS server behind a satellite
would silently fail to configure its own ports, with nothing anywhere saying why. That is the worst
available outcome and the one thing v1 must not ship.

---

## 16. Notes / implications

- **Credential replication & eventual consistency.** Each satellite holds a copy of the password set
  (same plaintext-in-config posture as one router today); edits/revocations propagate on sync — no
  instant network-wide revocation.
- **Core dependency.** The Core holds the only WAN, so a satellite is an island if the Core or its
  tunnels are down.
- **Performance ceiling.** All satellite traffic is software-encrypted on the K1 at both ends —
  comfortable for household use, a cap for sustained high-bandwidth transfer.
- **Security posture.** Inter-router links are cryptographically authenticated and encrypted; a
  physical tap on the cable cannot read traffic or inject into a profile.
- **Transparency is the driving constraint.** "One SSID, just type the password / just plug in" is
  what forces local resolution + central replication over any client-configured auth scheme.

```

---

## 17. Reconciliation with issue #4043 (2026-09-21)

This design predates the upstream feature request. Where they differ, the issue and
`SupportingEvidenceForSatelliteRouter.md` are authoritative; the differences are:

| Was | Now |
| --- | --- |
| **D6** role baked at flash, reflash to change | Role chosen at initial setup; changing it erases role state, factory-resets and reboots into the new role. Reuses `system.rs:742` (`firstboot`) rather than a second eraser. Switching in place is rejected because it makes every Core-only behaviour reversible mid-flight and turns an accidental change into two boxes that each believe they own the profiles. |
| Layer-2 extension listed as a rejected alternative | Reopened as **D14**, because mDNS is link-local and a routed satellite breaks `<hostname>.local` for StartOS servers, printers and IoT discovery *within a single profile*. Not yet decided. |
| No stated scale target | **D15**: benchmark one Core, two satellites, fifteen profiles. Expected distribution is 80–90% single-router; one or two satellites where used; 4–6 profiles typical, 8–12 power users, 10–15 technical/business. |
| StartTunnel not weighed | Weighed and set aside *as the transport* — a StartTunnel spoke is a host with one tunnel address, a satellite is a router advertising a subnet. Four of its parts are reuse candidates: WireGuard key/PSK handling (#3681), `WgSubnetConfig`'s per-segment DNS and egress, `tunnel/wg6.rs` routed IPv6 with no delegation protocol, and peer authorization by tunnel address plus public key (#3682). |
| Port count treated as a test-topology detail | Stated as a product limit: each satellite needs its own wired link to the Core, so current hardware supports exactly one satellite. |
| Concern "VLAN tag consistency across routers" | Considered resolved — a satellite is paired blank and receives its tags from the Core, so the question collapses into config sync. |

Two questions are parked for the implementation phase in
`satellite-router-open-questions.md`: backup/restore when a satellite holds a newer generation than
a restored Core, and the firmware-upgrade strategy across a Core and its satellites.
```
