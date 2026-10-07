# Satellite Router — Implementation Status & Next Steps

Companion to `satellite-router.md` (design) and `satellite-router-testplan.md`. Tracks what has
landed on the `start-wrt/satellite-router-v2` branch and what remains, in the design's phase order
(§8). This is a large feature spanning new backend modules, security-critical auth, WAN-less
networking, and a new Angular UI; it is deliberately staged.

**State as of 2026-09-21.** Filed upstream as `Start9Labs/start-technologies#4043`, awaiting a
maintainer response. The branch merges cleanly onto upstream `master` (`84734c818`) with one trivial
conflict in `web/.../i18n/dictionaries/en.ts`, where upstream's #3939 strings took IDs 555–557 and
the satellite Published-Ports string renumbers to 558. _Update 2026-09-28:_ against the
`start-wrt/v1.2.0` tag the conflict is the same, but #4095's Root CA strings now hold 558–559, so
the satellite string renumbers to **560**. Verified green in the capped container at
`opt-level 0`: **609 tests, 0 failures**, including all nine `satellite.rs` tests, five `vpn_site.rs`
tests and 31 in `port_control.rs`. Nothing has been exercised on two physical routers; the second
unit has not arrived.

`build/stage-files.sh` now lists `role.json` and `satellites.json` in the sysupgrade keep set —
without them a satellite forgot its role on update and booted as a Core, and a Core forgot its
pairings. The same list feeds `sysupgrade --create-backup`, so this is also what puts pairing into
the backup (D16).

The superseded `start-wrt/satellite-router` branch (pre-rebase duplicate, byte-identical satellite
files) is retired.

## Resume here (paused 2026-09-22)

Work is paused on two external dependencies, not on anything in the tree:

1. **A maintainer response to #4043.** The repo's triage workflow routes issue type `Feature` to
   `FEATURE_OWNER` regardless of project, so the request is assigned to the feature owner rather
   than to the StartWRT code owner who wrote most of the tracker. Both perspectives matter — one
   owns the product decision, the other owns the code this lands in.
2. **The second router.** Everything that would prove the design needs two boxes.

When picking back up, in order of value:

- **The hardware bring-up test** (`satellite-router-hardware-test.md`) the moment a second unit
  exists. WAN-less egress is the premise the rest of the design rests on and it is still unproven.
- **D14 (routed vs. bridged): talk to the StartWRT maintainers first** (decided 2026-09-28), then
  measure on that same two-router bench: VXLAN-over-
  WireGuard throughput on the K1, what MTU actually survives, and whether `.local` resolves across
  the boxes each way. For the bridged option, also cable a deliberate second path and time how
  long loop protection takes to catch it (design §6 risk 12). The issue asks the maintainers to
  rule on this; arriving with data is stronger than arriving with a question.
- **Then the phases below**, which are otherwise unchanged.

**An unrelated first contribution, if one is wanted while waiting.** #3862 (the delegated IPv6
prefix size is never read from netifd) is a real prerequisite for the IPv6 phase, and it is
testable without satellites — but only against a real prefix delegation. Where an ISP does not
provide one, a DHCPv6 server on the WAN port can delegate `/48`, `/56` and `/64` in turn, which is
_better_ than a live ISP for this particular bug: the `/64` case is the one that silently produces
no GUA on shipped defaults, and no ISP handing out a `/56` would let you reproduce it. Failing
that, #3681 (lift shared WireGuard key and PSK handling into `shared-libs/`) needs no network at
all and satellite pairing is a direct beneficiary.

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
  `generation`; `country` and the per-radio channel plan `radios`/`RadioPlan` added 2026-09-28 for D20), a pure free-UDP-port allocator (`allocate_listen_ports`), and the reserved
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
  (`{profiles, passwords, ports, ssid, country, radios}`; no admin password, D22) carrying a monotonic
  **generation** number. `country` is the Core's `wifi.get` country; `radios` is this satellite's
  part of the Core's channel plan (D20).
- ☐ Push transport: Core as RPC client over the management tunnel (reuse `CliContext::call_remote` /
  `registry::call_registry_rpc`) → a satellite `satellite.apply` endpoint.
- ☐ Satellite apply: regenerate local profiles via the existing `profiles.rs` rewrite chain against
  its own `/24`s; reject older generations; full-snapshot reconcile on reconnect + version heartbeat.
- ☐ Satellite Wi-Fi apply (D20): set the country and each radio's planned channel and `htmode`
  through the existing `wifi.rs` apply path; never `auto`. A country the satellite's regulatory
  database lacks refuses the snapshot (the D17 path); a channel its radio refuses takes the plan's
  next choice, else leaves that radio off, and is reported.
- ☐ Satellite reports upstream (the D13 channel): radio inventory at pairing; on request, a scan per
  radio (networks heard per channel with signal, channel busy time), taken while dark before the
  first plan.
- ☐ **Core channel planner (D20)** — new code, the largest piece D20 adds. Inputs: every router's
  scans and the pins; output: channel and width per radio, separating routers that hear each other
  well, then avoiding neighbours. Runs at pairing, on a country change and on request; shows the
  plan before applying. Once a Core has a satellite, its own radios take the plan instead of
  hostapd's automatic selection. Pure and unit-testable without hardware.
- ☐ D18 enforcement: an nftables file shipped under `/usr/share/` (not the auto-included
  `nftables.d/table-pre/`, which would load on a Core), loaded by an fw4 `config include` the daemon
  writes on a satellite only. Its empty set means dark, so an fw4 reload fails closed. Staged by
  `build/stage-files.sh` like the other `.nft` files.

## Phase 6 — Capability gating (D7) & UI

- ☐ Role-aware middleware: on a Satellite, make Core-authoring RPCs (`profiles`, `wifi`, `wan`,
  `vpn_server`, `published-ports`) read-only/disabled; keep `system`/`lan`/`devices` local.
- ☐ Angular "Satellites" surface (`web/`): pair dialog + registry/status list (pattern-match
  `routes/published-ports`); wire `api.service.ts` + `live-api` + `mock-api`; `app.routes.ts`/settings.
- ☐ Channel plan view: every router's radios with planned channel and width, a pin per radio from
  the Core's `wifi.regulatory` list, a re-plan button that previews before applying, and a warning
  when a pin overlaps a router it hears well.
- ☐ Pairing asks for the country when the Core has none (D20: the world subset leaves 5 GHz one
  80 MHz block, too little to separate more than two routers).
- ☐ D19 enforcement: the backhaul role writes a UCI fw4 zone (input and forward dropped, no DHCP)
  plus one WireGuard accept rule per satellite. Anything UCI cannot express follows D18's
  file-plus-include rule, loaded only on a Core with a backhaul port.

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

## Adoption and recovery (D6, D21, D22)

- ☐ Core announcement on every LAN port (IPv6 link-local, signed with the Core's key); a fresh
  router that hears it on its WAN port keeps setup Wi-Fi off and waits to be adopted.
- ☐ Core lists unadopted routers; on a port still serving a profile, asks "Make LAN port _n_ the
  satellite port?" listing the devices that will lose access, and changes nothing until approved.
- ☐ Adoption handshake keyed by the sticker Wi-Fi password (EEPROM) typed at the Core; pins both
  WireGuard keys; the password never crosses the wire in the clear. Needs the phase 4 security
  review.
- ☐ A fresh router broadcasts nothing until it has checked its WAN port (link, a DHCP attempt, one
  Core-announcement interval).
- ☐ A fresh router that hears no Core and gets no Internet on its WAN port says so on its setup
  page (no cable / cable but nothing answering / address but no Internet) and asks whether it is a
  satellite with a cabling fault or a main router to set up.
- ☐ Board with no EEPROM password: pre-adoption page on the satellite's LAN port at `router.lan`
  showing a one-time adoption code.
- ☐ Recovery mode: isolated recovery network on the LAN port and a `StartWRT-Recovery-<last 4 hex of WAN MAC>` 2.4 GHz SSID,
  (the satellite's sticker password, one client, 1 minute after going dark — placeholder); DNS answers `router.lan` with the satellite; page
  headed `Satellite <satellite-specific name> — recovery mode`, served over HTTP, no login;
  diagnostics, filtered logs, restart, factory reset.
- ☐ Factory reset on a satellite erases the overlay like the existing soft reset, and the reset
  router waits for adoption broadcasting nothing. The Core treats it as new and offers "use the saved
  configuration of _name_, or set up a new satellite" (also the path for replacing failed hardware).
  Saved configurations stay until removed by hand, with their last-connected date; adopting a new
  satellite offers a multi-select of unconnected saved configurations to delete, and deleting
  revokes the old key.
- ☐ Satellite status in the Core: state, last seen, firmware version when known, older-than-Core
  marker (D17); its recovery SSID; each port's profile and link state; Wi-Fi client count, in total and per profile
  (counts only; the device list is phase 8).
- ☐ User docs, as updates to the existing StartWRT book (`docs/src/`) with the code: building a
  Core with satellites end to end; the quick start (`initial-setup.md`) updated so adding a
  satellite takes no reading; a failure-scenarios section; `wifi.md`/`factory-reset.md` saying
  plainly what each sticker password opens and that replacing **Default** retires the Core's.
  The failure-scenarios section also covers a reset satellite with an empty WAN port, whose setup
  network is named `StartWRT` like an unrenamed main network.
- ☐ **Hardware watchdog, as a separate upstream change** (benefits every StartWRT router): enable
  the K1 watchdog in the device tree (disabled in the vendor `k1-x.dtsi`), and stop the vendor
  driver self-feeding once user space opens `/dev/watchdog`. Testable on one router.

## Cross-cutting / packaging

- ☐ `API_CONTRACT.md` updated as each endpoint lands; web `api.service` trio kept in sync (coupled-files rule).
- ☐ Verify `wireguard-tools`/`kmod-wireguard` in `build/openwrt.diffconfig` (D10).
- ☐ `CHANGELOG.md` + user docs (`docs/src/`) on user-visible completion. The docs must state the
  backhaul rule (design D19): the Core port that carries satellites, and any switch on it, is for
  satellites only, and any other device plugged in there gets no access by design.
- ☐ Future enhancement: **Wi-Fi backhaul** (design §11) — deferred.

## How to validate

- Per-commit: `make start-wrt-test` (container unit tests) — this environment has **no native Rust
  toolchain**, so the containerized path (`start9/cargo-zigbuild`) is the only compile route here.
- Feature acceptance: the Layer-3 hardware suite in `satellite-router-testplan.md` on a Core +
  Satellite pair. The **WAN-less egress (Phase 2)** is the highest-risk item and should be proven on
  hardware before the rest is built out.
