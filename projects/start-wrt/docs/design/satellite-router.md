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
more **Satellites** (followers, no WAN). **A router becomes a satellite by being adopted through a
Core** (D21); any router set up on its own becomes a Core. No one picks a role in a setup wizard, and
there is no runtime toggle: changing role is a factory reset (revised 2026-09-28). **Topology: hub-and-spoke** — each satellite
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
ports:[{port,vlan_tag}], ssid, country, radios:{radio→channel plan} }` — and each satellite
**regenerates its own**
subnet/DHCP/zone/routing locally by running the existing `profiles.rs` rewrite chain against its own
`/24`s. Pushing the _meaning_ (not raw UCI or a full backup) avoids clobbering satellite-local
identity (hostname, LAN IP, certs, WG keys, role). A satellite holds no admin password (D22). Satellites never author profile
state; they reconcile on reconnect after any missed edit.

---

## 3. Design Decisions (resolved)

| #   | Decision                                     | Resolution                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | Status                                     |
| --- | -------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------ |
| D1  | Profile model for multi-router subnets       | **Routed attachment** — single-`/24` profiles + attach remote subnets to the zone/table                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | ✅ locked                                  |
| D2  | Tunnel topology / profile separation         | **Option A — per-profile L3 tunnels** (~profiles×satellites; full zone reuse)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | ✅ locked                                  |
| D6  | Role selection & mutability                  | **Decided by adoption: a router added through a Core (D21) is a satellite; a router set up on its own is a Core.** The user is never asked to choose. Changing role is a factory reset, never an in-place switch (revised 2026-09-21, and 2026-09-28 for adoption)                                                                                                                                                                                                                                                                                                                                                      | ✅ locked                                  |
| D3  | Satellite Internet egress (no WAN)           | Reuse per-profile policy routing, with explicit **WAN-less** handling (endpoint via local link, DNS re-pointed, kill-switch neutralized)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | recommended                                |
| D4  | Pairing & remote-peer auth                   | Enrollment code → persistent per-pairing token, backed by `ed25519` identity, **bound to the management-tunnel source**; new auth path in `middleware/auth.rs`. **Needs security review.**                                                                                                                                                                                                                                                                                                                                                                                                                              | recommended                                |
| D5  | Config sync                                  | **Push a semantic payload**; satellite regenerates locally; reconcile on reconnect                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | recommended                                |
| D7  | Role capability gating                       | **Middleware** gate keyed on role (allowlist of satellite-local endpoints)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              | recommended                                |
| D8  | Subnet/VLAN coordination                     | `vlan_tag` **globally identical**; **Core-central** `/24` allocation per satellite; teach the two subnet guards about satellite subnets                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | recommended                                |
| D9  | DNS & DHCP split                             | **Satellite-local** DHCP and DNS per profile (mirrors the current per-gateway model)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | recommended                                |
| D10 | Packaging / image                            | WireGuard already ships; **no RADIUS**; verify `wireguard-tools`/`kmod-wireguard` only                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | recommended                                |
| D11 | Satellite IPv6 addressing                    | **Prefixes arrive as delegated state over the authenticated tunnel, never in the semantic payload** (§13). Mechanism: DHCPv6-PD on the management tunnel                                                                                                                                                                                                                                                                                                                                                                                                                                                                | invariant ✅ locked; mechanism recommended |
| D12 | Automatic port forwarding behind a satellite | **Satellite relays, Core authorizes** (§15); absent from v1 behind an explicit refusal                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | ✅ locked                                  |
| D13 | Satellite device registry                    | **Satellite reports device facts; Core owns per-device policy** (§14) — shared substrate for D11, D12, and the device UI                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | requirement ✅ locked; shape recommended   |
| D14 | Cross-router service discovery               | mDNS does not cross a routed boundary, so one profile stops being one discovery domain. Three candidates: layer-2 extension (VXLAN/GRETAP inside the tunnel), a per-profile mDNS reflector, or DNS injection as `start-core` already does for tunnel clients. **Open** — the problem is certain, the mechanism is not (added 2026-09-21). To be decided with the StartWRT maintainers before measurement settles it (2026-09-28). Layer-2 extension carries a loop-protection requirement (§6 risk 12)                                                                                                                  | ⬜ open                                    |
| D15 | Target scale                                 | Benchmark configuration is **one Core, two satellites, fifteen profiles**. One-LAN-port hardware serves several satellites through a switch on the Core's backhaul port (D19, §11); two satellites on one switch are unproven until a third router exists (added 2026-09-21, revised 2026-09-24)                                                                                                                                                                                                                                                                                                                        | ✅ locked                                  |
| D16 | Backup, restore and generation skew          | Pairing material is preserved across upgrade and included in the backup, relying on #3662's encryption. A Core restored behind its satellites **raises its own generation above theirs and pushes its restored content** rather than the satellites being reset; both ends say so. Satellites are not backed up (added 2026-09-22)                                                                                                                                                                                                                                                                                      | ✅ locked                                  |
| D17 | Firmware upgrade across a fleet              | Core first. **The sync payload is versioned, not the firmware**, and a version mismatch refuses to sync but never drops the peer — a satellite must keep its egress or it cannot download its own update (added 2026-09-22)                                                                                                                                                                                                                                                                                                                                                                                             | ✅ locked                                  |
| D18 | Satellite that loses its Core                | **Fail closed.** It stops broadcasting the SSID and stops serving its profile ports until its management tunnel is back **and** it has reconciled to the Core's current generation. Only the uplink stays up (§10) (added 2026-09-24)                                                                                                                                                                                                                                                                                                                                                                                   | ✅ locked                                  |
| D19 | Core backhaul port                           | **Dedicated to satellites.** A Core LAN port that carries satellites takes the satellite-backhaul role instead of a profile; one or more satellites share it through a switch; any other device on it reaches nothing (§11) (added 2026-09-24)                                                                                                                                                                                                                                                                                                                                                                          | ✅ locked                                  |
| D20 | Wi-Fi country and channels                   | **Country is the Core's, one per system, carried in the payload. The Core plans every router's channels and pushes them; a satellite never selects its own.** Each radio gets an explicit channel and width. The plan uses scans every router reports and runs at pairing, on a country change and on the admin's request; a channel pinned by hand is planned around (§18) (added 2026-09-28, revised the same day)                                                                                                                                                                                                    | ✅ locked                                  |
| D21 | Uplink and adoption                          | **A satellite's WAN port is always its uplink, and it plugs into a Core backhaul port (D19).** No other port is ever an uplink. Adding one is: cable the satellite's WAN port to a Core LAN port, power it on, open `router.lan`. The Core has already noticed it, and every step happens there; nobody connects to the satellite. **Trust: the admin types the satellite's sticker Wi-Fi password at the Core**; a board with none shows an adoption code on its LAN port instead (§19) (added 2026-09-28)                                                                                                             | ✅ locked                                  |
| D22 | Names and recovery                           | **`router.lan` is always the Core** from every router. **A satellite has no DNS name.** A dark satellite runs recovery mode, reachable on its LAN port and on a recovery Wi-Fi network, where it answers `router.lan` itself with a page headed `Satellite <satellite-specific name> — recovery mode` (a satellite named Garage shows `Satellite Garage — recovery mode`). Being on the recovery network is the only credential: the page holds no secrets and offers diagnostics, filtered logs, restart and factory reset, so a broken satellite never needs a reflash unless it cannot boot (§19) (added 2026-09-28) | ✅ locked                                  |

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
  manufactures all subnet/DHCP/zone/routing itself via the existing rewrite chain. _Revised
  2026-09-28:_ StartWRT 1.2.0's regulatory country adds `country`, and D20 adds the Core's
  per-radio channel plan for each satellite (`radios`, §18).

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
12. **Layer-2 extension brings back switching loops — CONDITIONS D14.** The routed design (D1, D2)
    is loop-free by construction: traffic between satellites crosses a routed hop at the Core, and
    no bridge spans two routers. Bridging a profile across the tunnels (D14's layer-2 candidate)
    gives that up. A second path between any two routers on the same profile then forms a loop: a
    customer switch cabled into Guest ports on two satellites, a wired backhaul plus a wireless one
    (§11), or two uplinks on one satellite. Hub-and-spoke alone does not prevent it, because the
    second path can be outside the design's control. _Field evidence (2026-09-24):_ a consumer mesh
    system with both satellites cabled to the primary had one satellite choose a wireless path
    through the other while its cable stayed up. The loop took down that part of the network. Users
    saw slow Internet across the whole house, because in-house clients roamed onto the affected
    satellites. Recovery needed an order-specific reboot and replug sequence.
    _Mitigation:_ if D14 picks layer-2 extension, loop protection is a **requirement**, not an
    option: spanning tree (or equivalent loop detection) on every profile bridge that has a
    VXLAN/GRETAP member, and a satellite that bridges a profile over exactly one path to the Core.
    It must recover on its own, with no reboot sequence. **The D14 bench measurement must include a
    deliberately cabled second path**, plus the time taken to detect and recover from it. The
    reflector candidate has a smaller version of the same problem ("two reflectors can loop").

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
for a cable/LAN backhaul with no NAT; keep it for Wi-Fi/NAT paths. The management tunnel keeps it
on every backhaul, because its liveness drives D18.)

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
traded away for the transparent-password UX — see D9.) D18 closes most of this window: a satellite
that cannot reach its Core admits no one, so a stale credential can be used only between the
tunnel coming back and the reconcile finishing, and D18 keeps the satellite dark through that too.

**A satellite that loses its Core fails closed (D18).** A satellite enforces policy the Core owns.
Cut off from the Core, it cannot know whether a password was revoked, a profile was deleted, or
a port was reassigned, so it must not keep admitting clients under what may be stale policy. It
also must not keep advertising a network that cannot reach the Internet. _Field evidence
(2026-09-24):_ in a consumer mesh system, satellites that lost their uplink kept broadcasting the
shared SSID. In-house clients roamed onto them and lost connectivity, and the owner saw slow
Internet across the whole house until the owner unplugged them.

- **Trigger.** The **management tunnel** stays silent past a liveness threshold of tens of seconds,
  not minutes. The management tunnel therefore keeps its persistent keepalive even on a cable
  backhaul, where the keepalive note above lets the data tunnels drop it. Liveness is judged by
  handshake age and keepalive traffic, not by the few-minute generation heartbeat. The exact
  threshold is a bench measurement.
- **Not a trigger.** The Core losing its **WAN**. The Core is still the policy authority, the
  satellite is still in sync, and local and cross-router LAN traffic still works. Losing Internet
  is the Core's problem, not a reason to take the satellite down.
- **What goes dark.** SSID broadcast on both bands, and forwarding and DHCP on every profile port.
- **What stays up.** The uplink port, the satellite's attempts to re-establish its tunnels, and
  recovery mode (D22, §19), so someone at the satellite can see why it is dark. _Revised
  2026-09-28:_ this said a status page "reachable over the uplink", which D19 made unreachable.
- **Resume condition.** The management tunnel is back **and** the satellite has applied the Core's
  current generation. A tunnel alone is not enough. Resume uses hysteresis, so a flapping backhaul
  does not flap the SSID. Under D17, a version mismatch keeps the satellite dark to clients while
  its management tunnel stays up to download its own update.
- **The cost, accepted.** While the Core is unreachable, devices on a satellite lose even local
  service, such as a printer in the same room. Failing closed is chosen over that convenience.
- **Where the enforcement lives.** The forwarding block is an nftables file shipped in the image,
  never written under `/etc`. StartWRT 1.2.0 (#4101) moved its own firewall includes out of
  `/etc/nftables.d` because sysupgrade restores that directory over the new image, so an old copy
  would shadow the new one after every update. The satellite's file follows the same rule, with
  one difference: it must not load on a Core, so it does **not** go in
  `/usr/share/nftables.d/table-pre/`, which fw4 includes on every router. It ships beside the
  others in `/usr/share/` and is loaded by an fw4 `config include` section (`type 'nftables'`,
  `position 'table-prepend'`) that the daemon writes only on a satellite. The include section
  lives in `/etc/config/firewall` and survives an update; the file it points at is replaced by
  every update.
- **Reload fails closed.** Assume an fw4 reload drops whatever the daemon added to the ruleset at
  runtime. The rule is therefore written so that its empty state is dark: forwarding on profile
  ports is allowed only while a daemon-maintained set says the satellite is live, and a reload
  that empties the set leaves it dark until the daemon re-affirms. #4006 took the same shape for
  outbound gateways: an empty table rejects rather than falls through.
- **The rest of D18 is not firewall.** SSID broadcast goes dark through the wireless config and
  DHCP through the dhcp config, both by the daemon. The radios return on the channels the Core
  planned (§18), so resuming never re-selects one. A DFS channel pinned by hand adds its radar
  check, a minute or more, to every resume that restarts the radio; whether disabling only the
  SSIDs avoids the restart is a bench measurement.

---

## 11. Backhaul medium (cable / LAN / Wi-Fi)

**The medium is a transport choice, not a security boundary.** All authentication and encryption live
in the WireGuard tunnel; the underlay is untrusted by design. The satellite therefore needs **no
particular port** and derives **no trust from the port or a Wi-Fi password** — exactly as intended.
Any path giving IP reachability to the Core's WG endpoint works.

- **Direct cable (recommended default)** — dedicated point-to-point link; best bandwidth/latency and
  most reliable. Because the Core is the only WAN, a robust backhaul matters.
- **Switched backhaul** — several satellites share one Core backhaul port through a switch. This
  is how one-LAN-port hardware serves more than one satellite. Traffic between satellites crosses
  the port twice, and all satellites share its gigabit. Neither matters for a home, where the WAN
  is the ceiling.
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

  **Loop and path-selection requirements for Wi-Fi backhaul.** The field failure in §6 risk 12 is
  exactly this feature going wrong: a cabled mesh satellite chose a wireless path through another
  satellite while its cable stayed up, and the resulting loop took down that part of the network
  until an order-specific reboot sequence cleared it. When Wi-Fi backhaul is picked up, it must
  hold to four rules:
  - **Associate to the Core only, never to another satellite.** A wireless hop through a peer is
    daisy-chaining, which §9 already excludes.
  - **One underlay at a time, cable preferred.** A satellite with a working cable does not also
    hold a wireless uplink. When the cable returns, it moves back to the cable automatically,
    without a reboot or replug sequence.
  - **The station interface is underlay only.** It carries the tunnels and is never a member of a
    profile bridge. In the routed design this makes a second underlay harmless, since WireGuard
    follows one endpoint and does not duplicate frames. Under D14's layer-2 extension it would be a
    second bridged path, which is a loop.
  - **Failover is tested, not assumed.** Pull the cable with Wi-Fi backhaul available, restore it,
    and confirm each switch happens once, without flapping and without a loop.

**The Core's backhaul port is dedicated to satellites (D19).** Whichever Core LAN port carries
satellites is assigned the **satellite backhaul** role instead of a Security Profile. It serves
satellites and nothing else. One or more satellites may sit on it through a switch, and hardware
with several LAN ports may have more than one backhaul port. Satellites reach the Core only through
a backhaul port, never through a profile port.

- **A port becomes a backhaul port only with explicit approval** (added 2026-09-28). Every Core LAN
  port starts out serving a profile. When the Core sees a new StartWRT router on one, it asks
  "Make LAN port _n_ the satellite port?" and lists the devices it currently sees on that port, which
  will lose access. Nothing changes until the admin approves. This holds on any hardware: a
  four-LAN-port router might carry a switch of satellites on port 1 and local devices on profiles on
  ports 2–4, and adding a satellite to the switch on port 1 must never silently cut off the rest.

- **A non-satellite device on a backhaul port gets nothing.** A laptop plugged into the backhaul
  switch, or into a cable unplugged from a satellite in the garage, gets no Internet access, no
  LAN access and no router administration. The only thing listening is the Core's WireGuard
  endpoint, and it does not answer unauthenticated packets. The Core does not hand out addresses
  on the backhaul to anything that asks. Link addresses are assigned at pairing. How an unpaired
  satellite reaches the pairing endpoint before then (link-local, or a lease that reaches the
  pairing endpoint alone) is an implementation-phase decision.
- **Why.** Backhaul cables run to where satellites sit: garages, outbuildings, the far end of a
  property. Such a cable is a jack anyone at that location can use. If the Core's port carried a
  profile, unplugging a satellite and plugging in a laptop would put the laptop on that profile,
  which by default is Admin.
- **What it costs.** The Core has no wired client port on one-LAN-port hardware, which the
  one-satellite layout already accepted. A device that needs a wired profile connection plugs into
  a satellite's profile port, or joins Wi-Fi.
- **Security.** The dedicated port adds no access path. What remains is availability: a device on
  a shared backhaul switch can disrupt the satellites on that switch (impersonating the Core's link
  address, flooding the segment), the way cutting a cable would, but never read or inject profile
  traffic. §12 threat 12.
- **UI.** The Ethernet page shows a backhaul port as "Satellite backhaul" in place of a profile
  picker. The Core should flag a device on the backhaul segment that is not a paired satellite.
- **User documentation must state the rule**: the backhaul port and anything plugged into it are
  for satellites only, and any other device plugged in there is cut off by design.
- **Where the enforcement lives.** Mostly UCI, because fw4 zones already express it: the backhaul
  interface gets its own zone with input and forward dropped, no DHCP server, and one accept rule
  per satellite for its WireGuard listen ports (the parameterized `ensure_wireguard_firewall_rule`
  from the risk list), all written by the daemon when a port takes the backhaul role. Anything fw4's
  UCI cannot express goes in an nftables file under the same rule as D18: shipped in the image under
  `/usr/share/`, loaded by an fw4 `config include` the daemon writes only on a Core with a backhaul
  port, and never under `/etc/nftables.d`. The pairing path for an unpaired satellite, still an
  implementation-phase decision, is the part most likely to need such a file.

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
| 5   | **Stale credential after revocation**                                                                                           | Reconnect-reconcile + generation numbers + staleness UI + **D18 fail-closed**: a satellite cut off from its Core admits no one until it has reconciled                               | **Low–Moderate; accepted** — bounded window, needs physical proximity, **no privilege escalation**; enterprises needing instant revocation use RADIUS (which we skip for transparency)             |
| 6   | **Physical theft of a satellite** (exposes plaintext PSKs, WG keys, token)                                                      | **One-click unpair revokes it at the Core instantly**; guidance to **rotate Wi-Fi passwords**; RPC firewalled to the tunnel                                                          | **Moderate; accepted** — same posture as today's single router (plaintext PSKs are unavoidable for the transparent-password model), extended to more devices; we don't target tamper-resistant APs |
| 7   | **Satellite management-RPC exposure**                                                                                           | Config-apply endpoint authorized **only over the authenticated tunnel** (token bound to source; firewall RPC to the tunnel), never from the LAN/Wi-Fi underlay                       | **Moderate** — a must-get-right; part of the D4 auth design                                                                                                                                        |
| 8   | Cross-profile isolation over the tunnel                                                                                         | Reused zone model (satellite separates profiles by VLAN; Core enforces cross-router forwarding)                                                                                      | Standard — mitigate with explicit isolation tests                                                                                                                                                  |
| 9   | Availability (Core down → satellite island)                                                                                     | Inherent to Core-only-WAN. **D18: the island fails closed** — SSID and profile ports go dark rather than serving stale policy or a dead network                                      | Availability property, not a breach; deliberately traded for safety                                                                                                                                |
| 10  | WG listen port on LAN/Wi-Fi                                                                                                     | WG silent to unauthenticated packets                                                                                                                                                 | Negligible                                                                                                                                                                                         |
| 11  | **Upstream channel abuse** (a paired satellite reporting bogus devices, or requesting forwards for addresses it does not serve) | Core authorizes, never the satellite (D12); every report and request bounded to subnets the Core allocated that satellite (D8/D13); arrival on satellite X's tunnel proves X sent it | **Moderate** — new trust direction; review alongside #2 and #7                                                                                                                                     |
| 12  | **Non-satellite device on the backhaul** (a laptop on the backhaul switch, or a satellite's cable replugged into a laptop)      | D19: the backhaul port carries no profile, the Core hands out no addresses there, and WireGuard is silent to unauthenticated packets                                                 | **No access.** Availability only: it can disrupt the satellites on that switch, as cutting the cable would                                                                                         |

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
| One satellite per Core on one-LAN-port hardware (issue form, D15) | Revised 2026-09-24: the Core's LAN port becomes a dedicated backhaul port (D19), and a switch on it serves several satellites. The issue form on #4043 still states the old cap. |
| Concern "VLAN tag consistency across routers" | Considered resolved — a satellite is paired blank and receives its tags from the Core, so the question collapses into config sync. |

Two questions are parked for the implementation phase in
`satellite-router-open-questions.md`: backup/restore when a satellite holds a newer generation than
a restored Core, and the firmware-upgrade strategy across a Core and its satellites.
```

---

## 18. Wi-Fi country and channels (D20)

StartWRT 1.2.0 added a regulatory country (`Points of Entry > Wi-Fi > Settings`, #3939). The country
sets which channels each band may use and the maximum transmit power; with none set a router runs
the world subset: 2.4 GHz channels 1–11, 5 GHz channels 36–48, 20 dBm. A standalone router's
automatic selection is hostapd's, run by each radio at start-up, skipping radar-detection (DFS)
channels (`acs_exclude_dfs`, written by `wifi.rs`).

**How many channels one router uses.** One per radio, at once. The Start9 router's Wi-Fi module
(AsiaRF AW7916-NPD) has two radios, and StartWRT runs them on 2.4 GHz (`radio0`, `HE20`) and 5 GHz
(`radio1`, `HE80`); the second radio can do 6 GHz instead of 5 but not both. So a router occupies
two channels, one per band, and a channel is a block as wide as its width: an 80 MHz 5 GHz channel
covers four 20 MHz channels, and an overlap check compares blocks, not channel numbers. The plan is
keyed by radio rather than band, so hardware with two radios in one band is covered without a
schema change.

**Country: the Core's, one per system.** A Core and its satellites stand in one place, so they
share one country. It travels in the payload as `country`; the satellite applies it through the
existing Wi-Fi apply path, which already waits for the regulatory domain to take effect. A satellite
has no country selector of its own. Unset on the Core means unset everywhere. A country the
satellite's regulatory database does not know is a firmware mismatch: it takes the D17 path, the
snapshot is refused and reported, and the management tunnel stays up so the satellite can update.

**Channels: the Core plans them all.** The Core is in control and satellites execute what it
sends, so automatic selection does not run on a satellite at all. For each radio on each router the
Core decides a channel and a width, and pushes them in the payload as `radios` (radio name → band,
channel, `htmode`). Once a Core has a satellite, its own radios are planned by the same planner
instead of by hostapd; a Core with none behaves exactly as today.

- **Inputs.** Each satellite reports its radio inventory at pairing (names, bands, the widths it
  supports) and scan results on request: the networks it hears on each channel and their signal,
  and each channel's busy time. The scans include the other StartWRT routers, which tells the Core
  how strongly each pair hears each other: two routers that barely hear each other can share a
  channel, two in adjacent rooms cannot. A satellite scans while it is still dark (D18), before
  any client is on it, so the first plan disturbs nothing. Reports travel over the management
  tunnel (the D13 upstream channel); the Core authorizes nothing from them.
- **Width is part of the plan.** It is the lever that makes room. With a country such as the US
  and DFS excluded, 5 GHz holds two 80 MHz blocks (36–48, 149–161) but four 40 MHz ones, so a Core
  and two satellites that hear each other well get 40 MHz each rather than two of them sharing an
  80 MHz block. 2.4 GHz stays at 20 MHz on 1, 6 and 11.
- **When it runs.** At pairing, when the country changes, and when the admin asks. Not
  continuously: moving a radio interrupts its clients, so a re-plan is an admin action, and the
  Core shows the proposed plan before applying it. Scanning a live radio briefly takes it off
  channel, which is a second reason re-plans are on request.
- **Pinned channels.** The admin can pin any radio on any router to a channel from the Core's UI,
  from the list the Core's `wifi.regulatory` returns for the system's country. The planner treats a
  pin as fixed and plans the rest around it, and warns when a pin overlaps another router it hears
  well.
- **A channel a satellite's radio refuses** falls back to the Core's next choice for that radio if
  the plan carries one, and otherwise leaves the radio off and reports it. It never falls back to
  local automatic selection, and it never refuses the whole snapshot, which would keep the
  satellite dark under D18 over one radio.
- **Deterministic after a power cut.** Satellites come back dark and bring their radios up only on
  the plan they last applied or the Core's current one, so routers restarting together cannot land
  on the same channel by racing each other.

**The country prompt at pairing.** Without a country, 5 GHz is 36–48 at 20 dBm, one 80 MHz block,
or two at 40 MHz. The planner can still separate a Core and one satellite, but not more. Pairing asks
for the country when the Core has none.

**The planner is new code.** hostapd's selection is per radio and local, so it cannot be reused
across routers; the Core needs its own planner and the scan reports to feed it. That is the largest
piece D20 adds to v1.

**Not in v1:** continuous re-planning, and fast roaming (802.11k/v/r).

---

## 19. Adoption and recovery (D21, D22)

**Addressing, restated.** Each profile on each router has its own `/24`, allocated by the Core (D1,
routed). The Core and a satellite are not in one range: the Core's Admin profile might be
`192.168.1.0/24` and a satellite's `192.168.11.0/24`, each router at `.1` of its own. The
inter-router links use `10.42.<n>.0/24`. Only D14's bridged option would put every router's profile
in one `/24`.

**The uplink is fixed (D21).** A satellite's WAN port is its uplink, always, and it plugs into a
Core backhaul port. StartWRT ties permissions to ports, so this is a hard-coded expectation, not a
setting.

**Adoption happens at the Core (D21).** The common case needs no direct connection to the satellite:

1. Cable the satellite's WAN port to the Core's backhaul port.
2. Power the satellite on.
3. Open `router.lan`. The Core has noticed a new router on its backhaul and offers to add it. Naming,
   trust, firmware and configuration all happen there.

This needs detection in both directions. The Core's LAN ports announce the Core on the link with no
addresses needed (IPv6 link-local), signed with the Core's key. A fresh router that hears it on its
WAN port keeps its setup Wi-Fi off and waits to be adopted, rather than offering setup to whoever
joins first. The Core in turn sees the unadopted router and lists it. If it arrived on a port that
still serves a profile, the Core asks before converting that port (D19).

**Trust at adoption (D21).** The admin types the satellite's sticker Wi-Fi password into the Core.
Start9 routers carry a unique password programmed into the board's EEPROM and printed on its
sticker; the satellite reads its own copy, so both sides can prove they know it without anyone
connecting to the satellite, and knowing it proves possession of that unit. The password is a
shared secret for the adoption handshake and never crosses the wire in the clear. The handshake
pins each side's WireGuard key.

**Boards with no sticker password.** A DIY board has no EEPROM password, and its setup is reachable
over Ethernet only (`installing.md`). While it waits to be adopted, its LAN port serves a local page:
the admin plugs a laptop into the satellite's LAN port, opens `router.lan`, and sees "Waiting to be
added to your StartWRT router" with a one-time adoption code, which they type at the Core. This is
the same isolated local page mechanism as recovery mode, in its pre-adoption state.

**`router.lan` is always the Core; satellites have no names (D22).** A satellite's DNS answers
`router.lan` with the Core's address for the asking client's profile, and the Core's certificate
already names `router.lan`. The name given to a satellite at adoption (garage, workshop) is a label
in the Core's UI and never a DNS name: `garage.lan` would share the namespace devices register
their hostnames in.

**Satellite status in the Core.** Each satellite's entry shows its state, when it was last seen, the
firmware version it runs when known, and whether that is older than the Core's (D17). A satellite on
older firmware is marked as such; its update shows as a step, and it stays dark to clients until it
matches. It also shows each of the satellite's ports (the profile assigned, and whether a link is up)
and how many Wi-Fi clients it serves, in total and per profile. These are counts the satellite
reports from its own port and association state; which devices they are is the device registry
(D13, §14).

**Recovery mode (D22).** A satellite that is dark (D18) serves a recovery network. There, and only
there, it answers `router.lan` itself, with a page headed `Satellite <satellite-specific name> — recovery mode`, using the name the satellite was given at adoption: a satellite named Garage shows
`Satellite Garage — recovery mode`. The recovery network reaches that
page and nothing else: no profile, no Internet, no forwarding, so it does not weaken D18.

- **Reached two ways.** The satellite's LAN port, and a recovery Wi-Fi network: an SSID starting
  `StartWRT-Recovery`, unique to each satellite, on the 2.4 GHz radio, on its last planned
  channel, protected by the satellite's own sticker Wi-Fi password, one client at a time. It comes
  up **1 minute** after the satellite goes dark, so a momentary blip does not flash it on and off.
  _The 1 minute is a placeholder, to revisit after bench and field experience._ A lost backhaul
  does not heal by itself, so the delay stays short.
- **Each satellite's recovery SSID is unique.** When the Core itself goes down, every satellite goes
  dark at once, and two in range must not look alike, because each takes its own sticker password.
  The SSID is `StartWRT-Recovery-<suffix>`, where the suffix is the last four hex digits of the
  satellite's WAN MAC address (for example `StartWRT-Recovery-4C8F`). The sticker does not show the
  MAC, so the suffix reveals nothing, and two routers in range sharing four hex digits is unlikely.
  It carries no hint about which password to use. Someone who picks the wrong one gets "incorrect
  password" and tries the next. The Core's satellite status shows each satellite's recovery SSID,
  so the admin can note it while things work. Devices
  hold no credentials for it, so none roams onto it, which is the failure D18 exists to prevent. A
  board with no sticker password has recovery over its LAN port only.
- **Being on the recovery network is the credential.** The page exposes no secrets, certificates
  or passwords; every action it offers is no worse than pulling the power, and a reset satellite
  still needs the admin to approve it at the Core. So there is no login, and a satellite never holds
  the admin password: the sync payload carries none. The page is served over HTTP like setup mode;
  a satellite never holds a certificate for `router.lan`, which a stolen one could use to
  impersonate the Core.
- **Why it is dark:** uplink link state, whether the Core's announcement is heard, management-tunnel
  handshake age, a version mismatch (D17), or a snapshot the satellite refused and why.
- **Logs, filtered to what helps reconnect.** Kernel messages, link and network state, the
  management tunnel, the firewall, and the daemon's own sync and apply messages: these explain why
  a satellite cannot reach or keep up with its Core. Client activity is left out: DHCP leases,
  Wi-Fi associations, device names and addresses. It explains nothing about the uplink, and leaving
  it out keeps the page focused and leaks less. Viewable and downloadable. The full logs remain
  available through the Core whenever the link is up.
- **Restart.** Most failures clear on a restart: pairing state survives it (D16), and so does an
  update.
- **Factory reset** clears everything it can: settings, role, pairing, keys, the channel plan and
  logs, as the existing soft reset erases the overlay. Only the firmware and the EEPROM sticker
  password survive. The satellite reboots as a fresh router, indistinguishable from a new one.

A reflash is only for a satellite that cannot boot.

**After a factory reset.** The reset is done from recovery mode, at the satellite. The router
reboots, hears its Core on its WAN port, and waits to be adopted **broadcasting nothing**: no shared
SSID, no recovery network, no setup network. The admin's phone or laptop drops off the recovery
network as it disappears and rejoins the shared SSID, which only the Core (and other healthy
satellites) now broadcast, so it cannot land on the reset satellite. The admin opens `router.lan`
there. If the reset router hears no Core, because its cable is the fault, it behaves as any fresh
router, with one difference: it knows it is stranded. Its WAN port hears no Core, and gets no
Internet either. Its setup page therefore opens with what it found on the WAN port rather than with
setup: no cable detected; a cable but nothing answering; or an address but no Internet. It then asks
the user to decide: "If this router is a satellite, check the cable from its WAN port to your main
router; it will join by itself once connected. If it is your main router, connect its WAN port to
your modem, or continue setting it up." It keeps listening, and once it hears a Core, with no setup
started, it takes its setup network down and waits to be adopted.

The Core does not recognise the unit: a reset satellite looks like a brand-new router, and is
adopted as one, sticker password included. The Core still holds the old satellite's entry (name,
ports, pins, subnets) with its old key, shown as unreachable. When it adopts a router it asks:
"Use the saved configuration of **Garage** for this router, or set it up as a new satellite?"
Reusing it moves the configuration to the new router and revokes the old key; the same choice
covers replacing a failed satellite with new hardware.

**Saved configurations are kept until removed by hand.** Each shows the date its satellite last
connected ("Garage — not connected since <date>"), and its subnets and ports stay reserved; nothing
expires on a timer, since a seasonal outbuilding may be off for months. When a router is adopted as a
**new** satellite rather than reusing one, the Core lists every saved configuration whose satellite
is not connected and offers to delete any of them; there may be several, so it is a selection, not
a yes/no. Deleting one revokes its key: a saved entry keeps its old key authorized, which is
harmless after a factory reset erased that key but not if the unit was stolen.

**What someone near a dark satellite sees.** A dark satellite broadcasts no shared SSID (D18). A
device near it sees its usual network only if the Core, or another satellite, reaches it; if one
does, it connects there, and otherwise its usual network is simply absent. It is never refused with
a wrong-password error, because nothing near it is offering that SSID. What it does see is the
recovery network. Wi-Fi gives an access point no way to explain a refusal before a device joins:
anyone who tries a profile password on the recovery network gets their device's generic "incorrect
password" message. The network name is therefore the only message a dark satellite can send, and the
user documentation has to carry the rest: "if your network disappears near a satellite and a
recovery network appears, the satellite has lost its Core."

**The sticker passwords, and what each one opens.** There are two, and they do different things.

- **The Core's sticker password** is, by default, the **Default** Wi-Fi password on the Admin
  profile (`wifi.md`): full access. That is upstream StartWRT behaviour, until the admin deletes or
  replaces that entry. Being one of the Core's passwords, it reaches every satellite through sync
  like any other.
- **A satellite's own sticker password** opens nothing in normal operation. A satellite's Wi-Fi
  carries only the Core's passwords, and nothing accepts a satellite's sticker password as a login.
  It is used for exactly two things: proving ownership at adoption, typed at the Core, and joining
  that satellite's recovery network while it is dark. A satellite sits in a garage or an
  outbuilding where anyone can read its sticker; reading it gives no access to any profile.

**A satellite that hangs.** Recovery mode needs a running system. For a hang, the K1 has a hardware
watchdog (`spacemit,soc-wdt`), and StartWRT's kernel builds its driver in
(`CONFIG_SPACEMIT_WATCHDOG=y`), but the vendor device tree it uses marks the watchdog disabled
(`status = "disabled"`, `spa,wdt-disabled` in `k1-x.dtsi`), so no router runs it today. Enabling it
is a device-tree patch in `openwrt-overlay/target/linux/spacemit/patches-6.18/`. The vendor driver
also feeds the watchdog itself from a kernel timer every 30 s, which catches a kernel hang but not a
user-space one; covering the latter needs the driver to stop self-feeding once user space opens
`/dev/watchdog`. This benefits every StartWRT router, not only satellites, and belongs upstream as
its own change.

**User documentation (with the feature).** Updates to the existing StartWRT book (`docs/src/`), not
a separate package, landing with the code: a guide to building a Core with satellites end to end; an
update to the quick start (`initial-setup.md`) so that adding a satellite takes a minute and no
reading; a failure-scenarios section: what a satellite that cannot reach its Core looks like, from
the Core and from beside it, and how each case is recovered; and, in `wifi.md` and
`factory-reset.md`, a plain statement of what each sticker password opens. The book already implies
that replacing the Core's **Default** password retires the sticker password until a factory reset;
it should say so outright.

**What a router broadcasts, by situation.** Checked 2026-09-28 against a network still using the
default SSID `StartWRT`, since a second router broadcasting the same name with another password would
make devices that saved it fail silently:

| Situation                                               | Broadcasts                                         | Collides with a network named `StartWRT`?                                                        |
| ------------------------------------------------------- | -------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| Adopted satellite, healthy                              | the Core's SSID                                    | no — it is the same network                                                                      |
| Adopted satellite, dark (D18)                           | `StartWRT-Recovery-<suffix>` only                  | no                                                                                               |
| Fresh or reset router that hears a Core on its WAN port | nothing, while it waits to be adopted              | no                                                                                               |
| Fresh router with Internet on its WAN port              | `StartWRT` setup network (upstream behaviour)      | only if the house already has another StartWRT network; that is a new Core, not a satellite case |
| Fresh or reset router with nothing on its WAN port      | `StartWRT` setup network, with the stranded prompt | **yes, the one residual case**                                                                   |

Two consequences. First, a router must not broadcast anything until it has looked at its WAN port:
link state, a DHCP attempt, and one Core-announcement interval. Otherwise a router plugged into a
Core broadcasts `StartWRT` for the seconds before it hears the Core. Second, the residual case needs
all three of a fresh or just-reset router, an empty WAN port, and an existing network still named
`StartWRT`; the person there has usually just reset that router themselves. Detection never adopts
by itself: "configured as a satellite automatically" means it waits, and adoption still needs the
admin's approval and the sticker password at the Core (D21).

**The residual case stays as upstream does (decided 2026-09-28).** A fresh or reset router with an
empty WAN port keeps the `StartWRT` setup SSID; the failure-scenarios section of the user docs covers
it: a reset satellite with no cable broadcasts a setup network also named `StartWRT`, and devices
that saved the main network under that name will fail to join it. The quick start is unchanged.
