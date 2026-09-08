# Satellite Router — Implementation Status & Next Steps

Companion to `satellite-router.md` (design) and `satellite-router-testplan.md`. Tracks what has
landed on the `start-wrt/satellite-router` branch and what remains, in the design's phase order
(§8). This is a large feature spanning new backend modules, security-critical auth, WAN-less
networking, and a new Angular UI; it is deliberately staged.

## Landed on this branch

- **Design** — `docs/design/satellite-router.md` (approved; decisions locked), this file, and the
  test plan.
- **Foundation module** — `backend/ctrl/src/satellite.rs`:
  - `RouterRole` (Core/Satellite) persisted to `/etc/startwrt/role.json` (default Core), with
    `load_role()` for other modules (e.g. the daemon gate) to consume.
  - Paired-satellite registry (`/etc/startwrt/satellites.json`) with add/remove + de-dup.
  - RPC `satellite.{get-role,set-role,list,pair,unpair,status}`, registered in `lib.rs::main_api`.
  - Core-only capability gating (`ensure_core`) — a first slice of D7.
  - Unit tests (5) for the pure logic.
- **Site-to-site config generation** — `backend/ctrl/src/vpn_site.rs` (Core side): a `wg`
  interface (transit `/32`), a **subnet-advertising** peer (the satellite's whole `/24` in
  `allowed_ips` + `route_allowed_ips=1`), profile firewall-zone membership, and a transit-zone
  accept rule; the `UciVpnSite` metadata section; `provision_core_site_tunnel` (pure config-writer,
  not yet called into effect); 3 tests incl. a parse→provision→assert integration test. In
  `vpn_server.rs`: `ensure_firewall_zone` exposed and a source-zone-parameterized firewall-rule
  helper added (existing `wan` callers unchanged).
- **Executable manual provisioning + satellite-side tunnel** — `vpn_site.rs`
  `provision_satellite_site_tunnel` (the satellite dials the Core, full egress-via-Core); and the
  RPC commands `satellite.provision-core-tunnel` / `provision-satellite-tunnel` that apply the
  generators to `/etc/config` and bring the tunnel up (role-gated). This makes the **basic
  hardware bring-up test executable** — runbook in `satellite-router-hardware-test.md`.
- **Satellite local profile serving** — `vpn_site.rs` `provision_satellite_local_profile`
  (interface `br-lan.<vlan>` + `/24` + DHCP pool + a LAN port on the VLAN + zone membership) and the
  `satellite.provision-satellite-profile` RPC. Lets a **downstream client** plugged into the
  satellite land on the profile and egress via the Core (runbook Step 5). Wi-Fi (per-PSK) entry and
  automatic sync of these from the Core are still follow-ups.
- **Config-sync contract + pairing primitives** — in `satellite.rs`: the semantic snapshot types
  (`SyncSnapshot`/`ProfileSpec`/`PasswordSpec`/`PortSpec`, camelCase, with a monotonic
  `generation`), a pure free-UDP-port allocator (`allocate_listen_ports`), and the reserved
  inter-router transit block + `transit_addrs(index)`. 4 tests.
- **API contract** — `API_CONTRACT.md` section for `satellite.*`.
- **Automatic port forwarding refused, explicitly (D12, phase 7)** — `port_control.rs`: a Satellite
  refuses every PCP/UPnP mapping request at `Via::is_known_client`, the single chokepoint the shared
  core consults before MAP, the SNI path, and all three UPnP actions. The client gets a PCP
  `NOT_AUTHORIZED` / UPnP fault 606 (both already emitted by the shared core), and the daemon logs
  it at `warn`, throttled to one line per client per 5 minutes. Paired with a note on the Published
  Ports page (`web/routes/published-ports/index.ts` + the `api.service` trio), because the automatic
  section is hidden when the list is empty — which on a Satellite it always is. 1 test.

> **Honest scope note.** `pair` currently records a satellite in the Core registry only; it does
> **not** yet establish tunnels, validate an enrollment token, or push config — and its response
> says so. Everything below is not yet implemented.

## Phase 1 — Role & provisioning (partly done)

- ✅ Role marker + `satellite.get-role/set-role`.
- ☐ **Daemon gate** — in `bins/daemon.rs::inner_main`, read `satellite::load_role()` and, when
  Satellite, **skip** the Core-only normal-mode block (`profiles::bootstrap_admin_profile`, WAN
  setup, `system::apply_remote_access`, schedule/cron regeneration).
- ☐ **Set role at flash** — capture the role choice in the setup wizard and write it in
  `setup.rs::run_setup_flash_inner` (extend `SetupStatusRes`); role is immutable post-flash (D6).

## Phase 2 — Site-to-site transport (`vpn_site.rs`, new) ★ top risk

- ✅ **Core-side config generation** (`vpn_site.rs`): subnet-advertising peer (prefix `allowed_ips`,
  not `/32`), transit-underlay addressing decoupled from the profile `/24`s, per-profile tunnel
  interface + peer + zone membership + accept rule; `UciVpnSite` metadata. Reuses `WgInterface` and
  the refactored `vpn_server` helpers; does **not** touch the `allocate_peer_ip`/proxy-ARP/`/32`
  host paths.
- ✅ Source-zone-parameterized WG accept rule (`vpn_server::ensure_wireguard_firewall_rule_in_zone`).
- ✅ **Satellite-side generator** (`provision_satellite_site_tunnel`) + **executable manual
  provisioning** (`satellite.provision-{core,satellite}-tunnel`, apply + `ifup` + reload). Enough to
  run the **basic hardware bring-up test** (`satellite-router-hardware-test.md`).
- ☐ **Automatic (paired) provisioning** — the pairing flow (D4) calls the same generators with
  allocated keys/ports instead of hand-entered ones.
- ☐ **Underlay automation** — the transit-port IP + `transit` firewall zone are operator-manual in
  the runbook today; automate at pairing.
- ☐ **MSS/MTU clamp** on the Core ingress; validate downstream-host (not just self-ping) egress.
- ☐ **Satellite side** — reuse `vpn_client.rs` interface/peer construction to dial the Core; add
  **WAN-less egress** (D3): pin the Core-endpoint `/32` via the local transit link (not WAN),
  re-point DNS, neutralize the kill-switch `unreachable` fallback. **Validate on hardware early.**
- ☐ MSS clamp / correct tunnel MTU on the Core ingress (none today; reuse the `mtu_fix` pattern).

## Phase 3 — Routed attachment at the Core (`profiles.rs`)

- ☐ Attach a satellite `/24` to its profile: add the tunnel interface to the `vlan_<iface>` zone
  (Vec `network`), and — for VPN-routed profiles — emit a source ip-rule (`src <remote/24> lookup
<vlan_tag>`) + a route in the per-VLAN table (generalize `prr_`/`plr_`).
- ☐ Extend `sync_cross_subnet_routes` / `sync_vpn_peer_cross_routes` to enumerate satellite subnets.
- ☐ Teach `guard_subnet_collision` / `validate_profile_block` about Core-central satellite subnet
  allocation (D8); keep `vlan_tag` globally identical across routers.

## Phase 4 — Pairing & remote-peer auth (`satellite.rs` + `middleware/auth.rs`) ★ security review

- ☐ Enrollment: single-use, short-lived, admin-initiated token; key-fingerprint confirmation;
  reuse `sign/ed25519` + `registry/device_info` for signed identity.
- ☐ New remote-peer auth path in `middleware/auth.rs` — a per-pairing token (à la
  `auth::HashSessionToken`) **bound to the management-tunnel source**; firewall the satellite's
  config-apply RPC to the tunnel only.
- ☐ Unpair must revoke at the Core immediately (drop tunnels + invalidate token).

## Phase 5 — Config sync (D5)

- ☐ Semantic payload types + a Core-side builder from `profiles`/`wifi`/`ethernet`
  (`{profiles, passwords, ports, ssid, adminKey}`) carrying a monotonic **generation** number.
- ☐ Push transport: Core as RPC client over the management tunnel (reuse `CliContext::call_remote` /
  `registry::call_registry_rpc`) → a satellite `satellite.apply` endpoint.
- ☐ Satellite apply: regenerate local profiles via the existing `profiles.rs` rewrite chain against
  its own `/24`s; reject older generations; full-snapshot reconcile on reconnect + version heartbeat.

## Phase 6 — Capability gating (D7) & UI

- ☐ Role-aware middleware: on a Satellite, make Core-authoring RPCs (`profiles`, `wifi`, `wan`,
  `vpn_server`, `published-ports`) read-only/disabled; keep `system`/`lan`/`devices` local.
- ☐ Angular "Satellites" surface (`web/`): pair dialog + registry/status list (pattern-match
  `routes/published-ports`); wire `api.service.ts` + `live-api` + `mock-api`; `app.routes.ts`/settings.

## Phase 7 — v1 refusal path (D12) ✅ done

- ✅ Satellite refuses PCP/UPnP explicitly (protocol error + `warn` log, throttled per client).
- ✅ Published Ports page states the feature is unavailable on a Satellite and points at the Core.
- ☐ **Not yet exercised on hardware** — the refusal is unit-tested and the servers are unchanged on
  a Core, but no PCP/UPnP client has been pointed at a Satellite. Fold into the hardware suite.

## Phase 8 — Device registry (D13)

- ☐ Satellite reports device facts upstream (the first satellite→Core direction; see design §14 and
  threat #11); Core owns policy and renders them in the device list. Unblocks the per-device toggle,
  D12's authorization, and IPv6 published ports at once.

## Phase 9 — IPv6 (D11) ★ gated on a spike

- ☐ **Run the risk #9 bench spike first** — DHCPv6-PD over a WireGuard interface has no precedent in
  this codebase (NOARP p2p device, no automatic link-local, DHCPv6 solicits to `ff02::1:2`). Two
  Linux boxes, `odhcpd` one side, a DHCPv6 client the other, `ff02::1:2` in `allowed_ips`. Needs no
  satellite and no K1. If it fails, fall back to Core-central `/64` allocation pushed as delegated
  state (design §13).
- ☐ Then: prefix delegation over the management tunnel + the interface-keyed `rule6` attachment.
  The v1 data shapes already carry v6 (`subnets`, `ip6assign`, `IpAddr`), so no schema migration.

## Phase 10 — Automatic port forwarding (D12, full)

- ☐ Satellite-side listener relaying to the Core authorizer. **Requires phase 8.** Replaces phase
  7's refusal. The `arrival_matches` substitute is a _replacement, not an inheritance_ — flag it in
  the phase 4 security review (design §15).

## Cross-cutting / packaging

- ☐ `API_CONTRACT.md` updated as each endpoint lands; web `api.service` trio kept in sync (coupled-files rule).
- ☐ Verify `wireguard-tools`/`kmod-wireguard` in `build/openwrt.diffconfig` (D10).
- ☐ `CHANGELOG.md` + user docs (`docs/src/`) on user-visible completion.
- ☐ Future enhancement: **Wi-Fi backhaul** (design §11) — deferred.

## How to validate

- Per-commit: `make start-wrt-test` (container unit tests) — this environment has **no native Rust
  toolchain**, so the containerized path (`start9/cargo-zigbuild`) is the only compile route here.
- Feature acceptance: the Layer-3 hardware suite in `satellite-router-testplan.md` on a Core +
  Satellite pair. The **WAN-less egress (Phase 2)** is the highest-risk item and should be proven on
  hardware before the rest is built out.
