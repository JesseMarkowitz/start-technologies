# Satellite Router — Implementation Status & Next Steps

Companion to `satellite-router.md` (design). Tracks what has landed on the
`start-wrt/satellite-router-v2` branch and what remains, in build order.

**Direction (2026-10-18, D25).** Satellites are thin access points (APs) on a tagged Ethernet
trunk, with one WireGuard management tunnel per AP (design §21). The routed design that preceded
it — per-profile tunnels, satellite subnets, WAN-less egress, satellite DHCP/DNS — was built,
bench-tested on two routers and is retired. Its implementation stays in this branch's history up
to `904d03f97` (also kept as the fork branch `start-wrt/satellite-router-routed`), and its bench
results stay in `satellite-router-open-questions.md` §22–26. `satellite-router-testplan.md` and
`satellite-router-hardware-test.md` describe that design's bench and are kept as its record; the
D25 bench checks are at the end of design §21 and below.

**State as of 2026-10-18.** Batch one written on top of `4eaad8860` (upstream `master`
`83c211441`). Capped container, `opt-level 0`: **685 + 18 unit tests, 0 failures** (the routed
code's tests went with it); rustfmt, prettier, `check:wrt`, `check:i18n:wrt` clean.
Upstream `#4043` has had no maintainer reply since it was filed; it still describes the routed
design and is left as is until maintainers reply or code lands. Two-router bench: the Start9 K1
is the Core, a BPI-F3 the AP, adopted end to end with D24 over the backhaul VLAN of a trunk.

## Landed on this branch

What carries into D25:

- **Role marker and registry** (`satellite.rs`): `RouterRole` in `/etc/startwrt/role.json`
  (the satellite's holds `coreEndpoint` and `coreKey` after adoption), the registry in
  `/etc/startwrt/satellites.json`, both in `build/stage-files.sh`'s sysupgrade keep set (D16).
  RPCs `satellite.{get-role,set-role,list,unpair,status}`.
- **Adoption** (`adoption/`, D21, D24): keyed-BLAKE3 challenge-response over a link-local UDP
  transport, Core announce / candidates / adopt, the satellite's candidate listener on its uplink
  port, 5 tries then a 60 s cooldown on both sides, `satellite.adoption.{candidates,adopt,code}`.
  Bench-tested: adoption replaces a hand-provisioned tunnel on both routers with new keys and a
  derived PSK. `nohostroute 1` on any tunnel whose endpoint host carries a scope (open questions
  §25).
- **Sync payload types** (`satellite.rs`): `SyncSnapshot` with `generation`, `country` and the
  per-radio `RadioPlan` (D20). The profile and port specs inside it change shape under D25.
- **Core-side fail-closed shape**: `vsb_` unreachable routes beneath satellite subnet routes
  (`vpn_site.rs`). The routes go with the routed design; the pattern — a daemon-affirmed state
  whose absence is closed — is what D18 and trunk gating reuse.
- **API contract** §18 and the generated bindings; the `Allow-Adoption` firewall rule on zone
  `wan`; changelog fragment `minor-added-satellite-adoption.md`.

What batch one removes (routed only):

- `vpn_site.rs`: per-profile site tunnels both sides, the satellite-local profile, the `vsb_`
  routes; `satellite.provision-{core,satellite}-tunnel` and `provision-satellite-profile`;
  `transit_addrs` and `allocate_listen_ports`; `PairedSatellite`'s per-profile tunnel list.
- `satellite.pair` (contract-only pairing; adoption replaced it).
- The D12 refusal in `port_control.rs` and its Published Ports note and dictionary string (ID 573
  in all five dictionaries).
- `SyncSnapshot`'s per-satellite subnet fields.
- Design §20 (satellite DNS) stays as a record; nothing implements it.

## Batch one — trunk and AP bridge (the data path)

No gating, no sync yet: prove that a client behind an AP is an ordinary client of the Core.
Adoption provisions the management tunnel in its D25 shape from the start, so the routed
per-profile tunnel never has an interim replacement; the tunnel's gating, liveness, time and
firmware behaviour are batch two. Removal of the routed code rides in this batch.

**Core.**

- ☐ **Trunk port.** A port role beside the profile assignment in `ethernet.rs`, which today reads
  one profile per port and ignores tagged memberships. A trunk port is a tagged member of every
  profile's `bridge-vlan` (`<port>:t`) and is not in VLAN 1 untagged (`ensure_vlan_filtering`,
  `profiles.rs`, makes every bridge port PRIMARY on VLAN 1). Profile creation and deletion keep
  the trunk's memberships current, so a new profile is on every trunk the moment it exists.
  Approval and the list of devices that lose access (D19) are UI work in a later batch; the
  RPC takes the port and refuses one that is the Core's WAN.
- ☐ **Backhaul VLAN.** The trunk's untagged side is a VLAN of its own, not a profile: a reserved
  tag outside the profile range, the trunk port PRIMARY on it, a Core interface on `br-lan.<tag>`
  with no IPv4, no DHCP, no router advertisements, IPv6 link-local only, in a zone that accepts
  the adoption port and the management tunnel's listen port and drops everything else. The
  adoption listener moves from the Admin bridge interface to it.
- ☐ Ethernet UI: the Ethernet page wipes every `bridge-vlan` section on save and lists only
  `br-lan` ports plus the WAN; it must preserve a trunk port's memberships and show the port as a
  trunk. `firstboot_config` hard-codes `eth0`/`eth1` and is left alone.

**AP.**

- ☐ **Uplink as trunk.** The uplink port (D21: the WAN port where the board has one) joins
  `br-lan` as a tagged member of every profile VLAN and PRIMARY on the backhaul VLAN. The `wan`
  interface, its DHCP client and the `wan` zone go: the AP has no WAN.
- ☐ **Profile VLANs without addresses.** For each profile in the payload, a `bridge-vlan` with
  the tag and a network interface on `br-lan.<tag>` with `proto none`: a bridge member hostapd
  and access ports can join, no address, no DHCP, no firewall zone. The AP's own Admin LAN
  (`192.168.0.1/24` today, the source of the bench collision) goes with the rest.
- ☐ **Wi-Fi.** The same wireless config the Core writes (`wifi.rs`: one `wifi-iface` per radio,
  per-password dynamic VLAN through `wifi-vlan` sections keyed by the profile tag), with each
  `wifi-vlan` pointing at the address-less profile interface. Until sync exists (batch three) the
  bench writes the SSID and passwords by hand with `wifi set`; the AP must not rewrite them.
- ☐ **Access ports.** Each non-uplink port PRIMARY on one profile VLAN, or no member of any.
- ☐ **DHCP guard, AP variant.** The shipped guard (`backend/nftables/ruleset-pre/`) drops DHCP
  replies and router advertisements switched between bridge ports, which on an AP drops the
  Core's. The variant exempts frames arriving on the uplink. Bench probe 2026-10-18 on the
  shipped kernel: a bridge-family table, raw payload matches and `vlan id` load without
  `kmod-nft-bridge`; every `meta` match (`iifname`, `ibrname`, `ibrpvid`) fails. The exemption
  needs the module, so **`kmod-nft-bridge` joins `build/openwrt.diffconfig` (D10)** and the
  variant ships with the image build. Until both bench routers run that image the AP runs no
  guard at all: the shipped guard is not staged on the 1.2.1 image either, so the bench sees
  the same thing a release would.
- ☐ Daemon role gate (D7, first slice): on an AP skip `bootstrap_admin_profile`, WAN setup,
  `apply_remote_access` and schedule regeneration in `bins/daemon.rs::inner_main`. The AP's
  allowlist comes in batch four.
- ☐ **Management tunnel, provisioned at adoption** (`mgmt_tunnel.rs`): one WireGuard interface
  per AP on the Core (`sat_<name>`, listening on the backhaul zone, peer endpoint-less), one on
  the AP (`core`, dialling `[fe80::…%br-lan.4094]:<port>` with `nohostroute 1`), inside addresses
  `10.42.<n>.1/.2`. Adoption on the AP side writes the base layout (uplink into the bridge, no
  WAN, no addresses) and the tunnel in one step. Until the tunnel carries management the AP's
  backhaul zone also admits SSH, for the bench; batch two removes that rule.

**Removal.** The routed list above, in the same batch, with the unit tests that covered it.
Keep `satellite.rs`'s role, registry and `SyncSnapshot` skeleton.

**Bench record, 2026-10-18.** Trunk, backhaul, announce on the backhaul and adoption over it
passed on the K1. Two defects found and fixed on the way: both backhaul zones rejected ICMPv6,
so neither end could resolve the other's link-local address (now a neighbour-discovery rule on
both roles, design §21); and the daemon's AP gate skipped the Core-only startup work on any
satellite, adopted or not, which cost an unadopted satellite its WAN-side SSH (now gated on
`coreKey`). The BPI was adopted while running the first build and became unreachable over the
network: no neighbour discovery on its backhaul, no address on any VLAN, no serial. Reflash.
Still to pass: the management tunnel's handshake, `satellite.apply` on the AP, and the client
checks below.

**Bench exit for batch one.** On the bench the K1's only LAN port is the trunk, so during this
batch the Core is reached over its Wi-Fi and its WAN-side management port, and the BPI over
link-local on the backhaul VLAN (`ssh -J wrt-bench root@fe80::…%br-lan.<tag>`). Pass:

- A wired client on a BPI access port and a Wi-Fi client on a BPI password lease from the Core
  in the right profile, reach the Internet, and appear in the Core's device list.
- `router.lan` from behind the BPI answers with the Core; `.local` names resolve across routers.
- A profile edit on the Core (new password, new profile) reaches the BPI's clients with no change
  on the BPI beyond the hand-written Wi-Fi config.
- Throughput through the BPI against the raw link (the ~400 Mbit/s tunnel ceiling of open
  questions §24 must be gone).

## Batch two — management tunnel and trunk gating

- ☐ **Management tunnel behaviour.** The tunnel batch one provisions gains: the Core initiating
  the handshake, so an AP rebooting with a stale clock reopens it (design §21); the Core answering
  NTP on its end (StartWRT runs no NTP server today); the AP's SSH-on-backhaul rule removed, with
  management reaching the AP through the tunnel alone.
- ☐ **Trunk gating (D18 shape).** A bridge-family nftables file shipped under `/usr/share/` and
  loaded by an fw4 include the daemon writes on a Core with a trunk, holding a set of live trunk
  ports; profile-tagged frames on a trunk port not in the set are dropped, both directions; the
  backhaul VLAN is never gated. Needs `kmod-nft-bridge` (the probe above). The daemon adds a port
  when the AP's tunnel handshakes and removes it when D18's liveness threshold lapses; an fw4
  reload empties the set and the daemon re-affirms. Fallback if the module cannot be carried:
  toggle the trunk's tagged memberships with `bridge vlan`, re-applied after every network
  reload.
- ☐ **AP fail-closed (D18).** An AP whose tunnel is down takes its SSIDs down and cuts its access
  ports from the bridge; recovery mode (D22) is batch five.
- ☐ Unpair revokes: tunnel removed both sides, the port leaves the live set, the AP's `coreKey`
  cleared so it re-enters candidate mode. Same-name re-adoption replaces the registry entry and
  the old tunnel (today it leaves the old tunnel and key behind).
- ☐ Firmware reach: the Core forwards an AP's traffic only to the update source it is itself
  configured with (`startwrt.system.registry`, `DEFAULT_REGISTRY_URL` as fallback) and the hosts
  that registry's asset URLs name, and resolves those names for it. Everything else from the
  tunnel is dropped. The Core-pushes-firmware option is deferred.

**Bench exit.** The design §21 checks: gating with a managed switch and a wired client on it
(unplug the AP, reboot the AP, reboot the Core, reload the firewall; time the close and the
reopen); an AP rebooted with its clock set behind gets its tunnel back without help; a client
behind the AP leases from the Core with the guard variant loaded and a rogue DHCP server behind
the AP reaches no other client. Needs an image with `kmod-nft-bridge` flashed on both routers.

## Batch three — config sync (D5)

- ☐ Payload (`SyncSnapshot`): SSID, broadcast flags, passwords with their profile tags, access
  ports keyed by the names the AP reported, country and the per-radio plan (D20), the update
  source, `generation`. No addresses, no DNS, no subnets.
- ☐ Core builder from `wifi`, `profiles` and `ethernet`; push over the management tunnel with the
  Core as RPC client (`CliContext::call_remote` / `registry::call_registry_rpc` pattern) to a
  `satellite.apply` endpoint the AP serves only from the tunnel (D4, D7).
- ☐ AP apply: rewrite wireless, the profile `bridge-vlan`s and access ports from the payload;
  reject an older generation; full reconcile on reconnect; version heartbeat. A payload whose
  profile set changes restarts the AP's network (accepted for v1).
- ☐ AP reports at adoption: port inventory (name, uplink, link), radios; on request a scan per
  radio taken while dark (D20 input). Radios come from the kernel (`iw phy`, board.json), never
  from UCI `wireless`: a stale `wifi-device` from an earlier detection survives there (the
  bench BPI carries one for a phy that does not exist), and a single-phy MT7915 serves one band
  at a time where the K1's MT7916 serves both.
- ☐ Wi-Fi apply per D20: country and each radio's planned channel and width, never `auto`;
  an unknown country refuses the snapshot; a refused channel takes the plan's next choice.
- ☐ Core channel planner (D20): pure, unit-testable; runs at adoption, on a country change and on
  request; preview before apply; pins honoured. Once a Core has an AP its own radios take the
  plan too.
- ☐ Adoption asks for the country when the Core has none (D20).

## Batch four — role gate and the AP's page (D7, D23)

- ☐ Role-aware middleware: an AP serves an allowlist — its link and tunnel state, last generation,
  its clients per port and radio with their profile, firmware, logs, restart, factory reset — and
  refuses everything else, absent not read-only.
- ☐ The AP's local page (D23): a separate minimal app sharing the recovery page's shell; "Satellite
  _name_" heading, link to the Core, tunnel state, clients, firmware against the Core's, logs.
  Open: whether it carries restart and factory reset itself or the Core issues them over the
  tunnel.
- ☐ Core UI: Satellites page (adopt with the D24 code or sticker password, candidates list,
  status, unpair, saved configurations), the trunk approval dialog listing devices that lose
  access (D19), the channel plan view with pins and re-plan preview (D20).
- ☐ Device list: an AP's Wi-Fi clients show as Wi-Fi on the AP, not "Ethernet" on the Core (D13
  revised: the AP reports which radio or port each client is on; the Core already has the
  leases).

## Batch five — recovery and the rest of adoption (D22)

- ☐ A fresh router broadcasts nothing until it has checked its uplink port; one that hears no
  Core and gets no Internet says so on its setup page and asks satellite-or-main.
- ☐ Recovery mode: `StartWRT-Recovery-<last 4 hex of uplink MAC>` 2.4 GHz SSID and an isolated
  recovery network on an access port (a Wi-Fi-only AP has only the SSID), HTTP, no login, headed
  `Satellite <name> — recovery mode`, filtered logs, restart, factory reset. An AP has no
  address outside its tunnel and no console, so any fault in its backhaul layout leaves a
  reflash as the only way back until this exists (the bench's 2026-10-18 casualty, batch one).
  The network restart after `adopt_self` is the moment that layout first runs; recovery must
  come up on a layout that failed to bring the tunnel up, not only on a lost Core.
- ☐ Factory reset on an AP: overlay erased, waits for adoption broadcasting nothing; the Core
  offers "saved configuration of _name_" or new; saved configurations kept until removed, with a
  multi-select delete at adoption; deleting revokes the old key.
- ☐ Core status per AP: state, last seen, firmware and older-than-Core marker (D17), recovery SSID,
  each port's profile and link, Wi-Fi client counts per profile.
- ☐ Factory password on boards without one: deferred (design §21). Today's behaviour stays.

## Packaging, docs, upstream

- ☐ `build/openwrt.diffconfig`: `kmod-nft-bridge` (batch one). `kmod-wireguard` is already
  there. Start9's AP board needs its own image target when the board exists.
- ☐ `build/stage-files.sh`: the AP guard variant and the gating file under `/usr/share/`; keep
  set unchanged.
- ☐ Loop protection (design §6 risk 12, a v1 requirement): every bridge with a trunk port runs
  STP or an equivalent that recovers without a reboot; bench check with a deliberately cabled
  second path. Wi-Fi backhaul stays out until §11's rules hold.
- ☐ `API_CONTRACT.md` §18 and the generated bindings with every endpoint; the web `api.service`
  trio together.
- ☐ Changelog fragment updated to the AP shape; user docs as updates to the StartWRT book:
  building a Core with APs, the quick start, failure scenarios, the backhaul rule (a trunk port
  and any switch on it serve APs only; a managed switch on the trunk loses the Core while its AP
  is down), what each sticker password opens.
- ☐ Hardware watchdog as a separate upstream change (K1 watchdog disabled in the vendor dts;
  `/dev/watchdog` absent on the bench).
- ☐ Deferred: MACsec on the trunk, Core-pushes-firmware, the role's name, Start9's AP hardware.

## How to validate

- Per change: `make start-wrt-test` in the capped container (`--cpus=4 -m 10g -j 2`,
  `opt-level 0`; see the workspace notes), `make start-wrt-format-check`, `npm run check:wrt`,
  `make start-wrt-rpc-bindings-check` after any RPC change.
- On the bench: the exit checks of each batch above, driven from the client's side and with
  evidence from both routers. Batch two and the guard variant need an image build
  (`make start-wrt-image`, hours) flashed on both routers; everything in batch one runs on the
  1.2.1 image with the branch binary deployed over SSH.
