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
  WAN, no addresses) and the tunnel in one step. Batch one admitted SSH on the AP's backhaul for
  the bench; batch two removed that rule.

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

Written and bench-tested 2026-10-18 (`liveness.rs`, `bridge_filters.rs`, `control.rs`,
`update_reach.rs`; API contract §18). Passed on the two-router bench, both on the 1.2.1 image:
adoption with the new AP layout (tunnel up in seconds, endpoint and port learned, `live`); NTP
from the Core (2 ms offset), DNS through the Core, HTTPS to the registry allowed and HTTPS to
anything else refused; the payload applied; fail-closed (`ip link set sat_s1 down` on the Core:
the AP went dark 37 s later, the Core showed it dark, and 12 s after the link came back the AP
resumed); `unpair` released the AP, which reappeared as a candidate on its backhaul and was
adopted again with a new code, with the reach rules going and coming back. A laptop on the AP's
LAN jack leased from the Core's guest profile and showed in the Core's device list as an
Ethernet client; the same laptop on the AP's SSID with the Admin password leased from Admin.
The first build flapped dark every few minutes (the keepalive starvation above); with the
echoes, nine minutes of steady two-way traffic with no false dark, and the cut-and-restore
cycle repeated: Core dark after 47 s, AP dark after 50 s, Core up 2 s after the link returned,
AP resumed 20 s after. Not yet proven: the gate and the guard variant (no module in the 1.2.1
image), the stale-clock reboot. The bench reached the released AP over link-local SSH through
a rule added by hand on the AP for the test and removed after; the product path for that is
recovery mode.

- ☑ **Management tunnel behaviour.** Both ends can initiate: the AP listens on a fixed port
  (51899, admitted on its backhaul zone) and the Core's liveness loop records the endpoint each
  handshake comes from (`PairedSatellite.endpoint`) and re-pins it with `wg set` whenever the
  kernel has none, so an AP rebooting with a stale clock is reached from the Core's side. The
  Core's sysntpd serves (`system.ntp.enable_server`) and the AP's only time source is the Core's
  tunnel address; the tunnel addresses share a /24 so the Core's dnsmasq (`localservice`) answers
  the AP, whose resolver is the Core (`list dns` on the tunnel interface). The AP's default route
  is the tunnel (`allowed_ips 0.0.0.0/0`). The SSH-on-backhaul rule is gone; the bench reaches
  the AP as `ssh -J wrt-bench root@10.42.<n>.2`.
- ☑ **Trunk gating (D18 shape).** `bridge_filters::sync_core` writes
  `/usr/share/nftables.d/ruleset-pre/20-startwrt-trunk-gate.nft` on a Core with a trunk (at
  boot and on every `ethernet.set`), a bridge table with an `ifname` set `live` and two
  chains: `vlan id != 4094 iifname != @live drop` at prerouting and the `oifname` mirror at
  postrouting. No trunk set is needed: on a Core, tagged frames exist on trunk ports alone.
  The liveness loop (5 s) sends the AP an echo inside the tunnel every tick and judges it by
  receive-counter progress or a fresh handshake within 45 s, maps its endpoint to a port through
  `ip -6 neigh` and `bridge fdb`, and rewrites the set in one `nft -f` transaction every tick,
  which also re-affirms after an fw4 reload. The echo is what makes the judgement sound: a bench
  hour of flapping showed that WireGuard re-arms a peer's keepalive timer on every packet it
  receives, so of two ends with the same interval one sends every keepalive and the other none,
  and the sender sees silence. Without
  `kmod-nft-bridge` (the bench K1 today) the gate is not written and a warning says the trunk
  stays open; the `bridge vlan` fallback was not built, since the module is in the image config.
- ☑ **AP fail-closed (D18).** The AP's loop watches its `core` tunnel the same way, echoing the
  Core's tunnel address every tick. Dark: `wifi
down` and `ip link set <port> down` on every bridge port that is neither the uplink nor a
  radio. Resumed after the tunnel has held for 10 s: ports up, `wifi up`. The daemon starts in
  an unknown state and acts on its first reading, so a boot without the Core goes dark within a
  tick. The reconcile-before-resume half of D18 waits for batch three.
- ☑ Unpair revokes: `satellite.unpair` sends `Release` over the tunnel to UDP 8720 at the AP's
  tunnel address (`control.rs`; the AP trusts the Core's tunnel source address, which WireGuard's
  cryptokey routing binds to the Core's key) and waits 2 s, then removes the Core end. The AP
  removes its tunnel end, clears `coreKey`/`coreAddress`/`coreEndpoint`, keeps its base layout,
  stays dark, and restarts the candidate listener on its backhaul VLAN. Same-name re-adoption
  keeps the tunnel's addresses and port. The port leaves the live set at the next tick.
- ☑ Firmware reach: fw4 ipsets `sat_reach4`/`sat_reach6` (`match dest_ip`, load files under
  `/etc/startwrt/`, kept across sysupgrade) and two rules passing TCP 443 from zone
  `satellites` to `wan` through them, written at adoption and removed at the last unpair. The
  Core's loop resolves the registry host plus every asset URL host of the versions the registry
  lists for it, every 10 minutes. Nothing else from the tunnel zone is forwarded. The AP's own
  update code still reads `startwrt.system.registry` locally; the payload's `updateSource` takes
  over in batch three.
- ☐ Not built: the `bridge vlan` gating fallback; the AP's DHCP guard variant is written by
  `sync_ap` only when the module loads, else the shipped guard is removed on the AP.

**Bench exit.** The design §21 checks: gating with a managed switch and a wired client on it
(unplug the AP, reboot the AP, reboot the Core, reload the firewall; time the close and the
reopen); an AP rebooted with its clock set behind gets its tunnel back without help; a client
behind the AP leases from the Core with the guard variant loaded and a rogue DHCP server behind
the AP reaches no other client; `unpair` releases the AP and it is adopted again from its
backhaul. Gating and the guard need an image with `kmod-nft-bridge` flashed on both routers;
the rest runs on the 1.2.1 image.

**Bench record, 2026-10-19, on the image.** Both routers were upgraded in place (`sysupgrade`,
settings kept) to the image carrying `kmod-nft-bridge`. The Core wrote the gate at boot, closed
with an empty set, and held the trunk port in the set within a tick of the first handshake.
Flushing the set by hand dropped every ping to a wired client behind the AP on the egress rule;
the next tick restored the port. With the AP's tunnel interface taken down for 70 s the Core
logged the AP dark 60 s after the last receive, dropped nine profile frames while closed, and
reopened two seconds after the first handshake back; the AP, whose missing interface counts as
dead at once, opened recovery 70 s in and resumed 12 s after the Core returned. The AP's guard
is the variant with its uplink exempted. Two lessons. The pre-release images' keep lists did
not carry the role, registry, generation, reach and applied files, so an upgrade off them loses
them once; the new keep.d carries them, and the Core floors its generation at the highest one an
AP reports in case its own is lost. And a gate file removed on a router that stopped being a
Core left its table in the kernel, closed, because fw4 reloads only its own table; `remove_gate`
now deletes the table. Later the same morning, the stale-clock reboot: the AP's clock was set a day
back, saved to its RTC, and its boot-time clock fix disabled so the stale time survived the reboot.
It booted a day behind, the tunnel was back 43 s after the reboot command with no help (the Core's
initiation, since the AP's own carried a timestamp older than the Core's last), the AP resumed 20 s
later, and its NTP corrected the clock 35 s after that; the Core never marked it dark and the trunk
gate never closed, the outage being shorter than the threshold. And the rogue DHCP server: a dnsmasq
in a network namespace on a veth port of the AP's bridge, on the same profile VLAN as a client in a
second namespace. The client leased from the Core every time; the rogue's offers and its NAKs of the
Core's lease all landed on the guard's drop counter and none reached the client. With the guard
table deleted the client still took the Core's offer, which arrived first, so the control shows the
race and not a poisoning.

**Bench record, 2026-10-21, managed switch.** An eight-port 802.1Q switch sits between the Core's
trunk port and the AP: the backhaul VLAN untagged on both router ports with it as their PVID, Admin
and every profile VLAN tagged on both, and each device port untagged in one profile's VLAN with that
VLAN as its PVID. The switch refuses to take a port out of the VLAN its PVID names and turns the
request into untagged membership instead, so a port is assigned by setting its PVID first and
removing it from VLAN 1 after; the switch's own management address stays on VLAN 1, which is Admin.
A laptop on a profile port leased from the Core in that profile, and the Devices list showed it as
Ethernet with the right profile, following the cable from one profile port to another (the old
entry lingers until its neighbour entry expires). A profile created on the Core while the switch was
live reached the AP over sync, and its VLAN was then added on the switch. The gating check: the AP
unplugged at the switch, the Core marked it out of contact and closed the trunk 62 s later, and the
laptop on the switch lost the Core at that moment; the AP plugged back in, in contact 1 s after its
tunnel returned, and the laptop back 2 s later. The switch's own loop prevention was off for the
loop-guard runs. The consequence §21 predicts held: on a Core whose only LAN port is the trunk,
every wired device on the switch loses the network whenever the AP is down, which is why a
general-purpose switch trunk is a separate decision.

## Batch three — config sync (D5)

Written 2026-10-18 (`sync.rs`, the tunnel API in `control.rs`; API contract §18).

- ☑ Payload (`SyncSnapshot`): SSID, broadcast flag, country, every password with its profile's
  VLAN (a password with no profile is Admin), every profile, the AP's access ports (only ports
  it reported), its radio plan, the Core's update source, `generation`.
- ☑ Core builder (`sync::build`) from `wifi::get`, the profile list and the registry's port
  assignments (`satellite.set-ports`). Push is derived, not hooked: every liveness tick rebuilds
  every AP's payload, hashes them, bumps the generation (`/etc/startwrt/sync.json`) when the
  digest moves, reads each live AP's status over the tunnel and pushes to any AP behind, with a
  30 s back-off after a failure. That covers the event push, the reconnect reconcile and the
  heartbeat with one mechanism. The Core is an HTTP JSON-RPC client of the AP's tunnel API
  (`http://<tunnel addr>:8720/rpc`, methods `apply`, `status`, `release`), which answers the
  Core's tunnel address alone; the UDP control channel of batch two is gone and the public
  `satellite.apply` RPC with it.
- ☑ AP apply: rejects an older generation, treats the applied one as done, rewrites and reloads
  as before. A payload whose profile set changes still restarts the AP's network (accepted).
- ☑ AP inventory in every status answer: ports from `/sys/class/net` with link and uplink
  flags; radios from UCI `wireless` `path` resolved to a phy (`+n` = the n-th phy on the device,
  so the bench BPI's stale `radio1` is absent) with the bands `iw phy` lists. The scan per radio
  while dark (D20 input) is not built.
- ☑ Wi-Fi apply per D20: country and each planned radio's channel and width. Not built: refusing
  an unknown country, falling back to the plan's next channel on a refusal.
- ☑ Core channel planner (`sync::plan`, pure, tested): per AP radio, a band (a dual-band phy
  takes 2.4 GHz first, a second radio the other band) and the least-used channel among 1/6/11
  at HE20 or 36/40/44/48 at HE80, counting the Core's own radios and every other AP's plan. It
  runs every tick as part of the build, so a country or channel change on the Core replans. Not
  built: scans, pins, preview, the Core's own radios taking the plan.
- ☑ D18's second half: an AP resumes only after the tunnel has held 10 s **and** it has applied
  the generation the Core's last status call named.
- ☑ Adoption refuses while the Core has no Wi-Fi country (D20). The bench K1 was given one.

**Bench record, 2026-10-18.** On the two-router bench the first tick after deploy read the
BPI's inventory (two ports, one dual-band radio; the stale `radio1` absent) and pushed
generation 2; `set-ports s1 eth0=101` pushed generation 3 fourteen seconds later. The BPI then
carried the Core's SSID and country, all six of the Core's passwords on their VLANs (the
hand-written batch-one payload had carried one of the guest profile's three), every profile
VLAN tagged on its uplink, the LAN jack on guest. The planned channel did not land at first:
`write_wireless` looked the plan up by wifi-iface name instead of radio name, a batch-one slip
the hand payload never reached; fixed with a test, and the next generation moved the BPI from
channel 1 to the planned 6, clear of the Core. A push that succeeded no longer holds the next
one back 30 s (seen as generation 5 waiting on generation 4's back-off), and the AP answers a
push before its reload restarts the tunnel interface, which had read as a failed push at the
Core. A phone then joined the AP with the third of the guest profile's passwords, the one the
hand payload had lacked: hostapd assigned it VLAN 101 from that password and the Core leased
it in guest through the trunk. Not yet proven: resume gated on the generation (the tunnel was
never cut on this build), re-adoption with the country check.

## Batch four — role gate and the AP's page (D7, D23)

Written 2026-10-18 (`middleware/role.rs`, `ap_page.rs`, `web/src/app/routes/access-points/`).

- ☑ Role gate: `RoleGate` runs ahead of session auth on the daemon's RPC server; on an adopted
  AP every method outside `satellite.get-role`, `satellite.status`, `satellite.adoption.code`,
  `system.info`, `system.restart`, `auth.logout` is refused. Not built: factory reset (batch five).
- ☑ The AP's page: server-rendered by the daemon at every path on an adopted AP, in place of the
  embedded UI (no second Angular app: the page is a few kilobytes and recovery mode reuses it).
  Heading "Satellite _name_" (the name is now in the role marker), link to `router.lan`, tunnel
  state, generation applied against the Core's, firmware against the Core's (both come with the
  Core's status calls), ports, radios, devices with their profile (from the applied payload kept
  in `/etc/startwrt/applied.json`), the last 40 daemon log lines. No actions: still open whether
  restart and factory reset live here or come from the Core over the tunnel.
- ☑ Core UI: an **Access Points** page under Points of entry — adopted APs with contact state,
  trunk port, generation, radio plan, per-port profile selector (`satellite.set-ports`), device
  count and a Remove action with confirmation; candidates with an Adopt dialog taking the name and
  the code or sticker password. The Ethernet page's profile selector gained "Trunk to access
  points" behind a confirmation (D19), and a trunk can be turned back into a profile port. Not
  built: the list of devices that lose access in that confirmation, saved configurations (batch
  five), channel pins and the re-plan preview (no pins in the backend yet).
- ☑ Device list: the AP reports its clients per port and radio in every status answer (`clients`
  in `TunnelStatus`, kept in the registry); the Core's device list shows such a device as
  "Wi-Fi 2.4GHz (garage)" or "Ethernet (garage)" instead of Ethernet on the trunk port.

**Bench record, 2026-10-18.** On the BPI, `wifi.get` over HTTP answers "not available on an
access point; use its Core" while `satellite.status` still asks for a session; the CLI on the
box is not gated, since it runs handlers in-process as root. The BPI's address serves the page
with its state, generation, firmware, ports, radios and the wired laptop with its VLAN; the
first build showed "Satellite satellite" because the BPI had been adopted before names were
recorded, so the Core's status call now carries the name; with that, and a generation applied
on the new build, the page read "Satellite s1" with the laptop in guest. The Core's device list shows the
laptop behind the BPI as "Ethernet (s1)"; `satellite.list` and the candidates list answer
through the UI's login path with inventory, clients and the plan. The Access Points page and
the trunk confirmation were type-checked and built, not clicked through: the bench has no
browser.

## Batch five — recovery and the rest of adoption (D22)

Written 2026-10-18 (`recovery.rs`; API contract §18).

- ☑ A router with no password yet runs the candidate listener on its WAN port whatever its role
  (`adoption::start`), so a fresh or reset router that hears a Core waits to be adopted; its
  setup page carries a banner saying so, with the adoption code, and offers "create a password"
  as the way to become a main router instead. Not built: the "no cable / nothing answering / no
  Internet" diagnosis on the setup page.
- ☑ Recovery mode: 60 s into the dark the AP moves its access ports to VLAN 4093 with
  `192.168.255.1/24`, DHCP and `router.lan` from its own dnsmasq, a zone that forwards nowhere;
  with a sticker password, `StartWRT-Recovery-<last 4 hex of the uplink MAC>` on the 2.4 GHz
  radio, psk2 with that password, one client, every profile SSID disabled. A second axum server
  bound to the recovery address (reuse-port beside the daemon's wildcard listener) serves the page
  headed "Satellite _name_ — recovery mode": uplink link, whether the Core's link-local answers,
  tunnel handshake age, firmware against the Core's, the last refused payload; Restart and
  Factory reset (`firstboot -y`) behind confirmations; the log filtered to kernel, netifd,
  WireGuard, firewall and daemon lines with DHCP and hostapd client lines dropped, viewable and
  downloadable. Resume restores the applied payload's layout (`/etc/startwrt/applied.json`) and
  removes every recovery section. Recovery runs on the dark-state machine, so it also covers an
  adoption whose layout never brought the tunnel up. Not built: the 1-minute delay is still the
  placeholder; the Core's own radios do not take the recovery SSID into the plan.
- ☑ Saved configurations: re-adopting a name keeps that entry's port assignments; the adopt dialog
  lists access points out of contact as names to take over; Remove on the Access Points page is
  the deletion and revokes the key. Not built: a multi-select delete inside the adopt flow.
- ☑ Core status per AP: firmware with an "Older than the Core" badge (D17), the recovery SSID
  shown while out of contact, each port's assignment and link (already), device count. Not built:
  Wi-Fi client counts per profile as a column.
- ☑ D17 replaced "dark until its firmware matches" (2026-10-19); see Fleet updates below. The
  original bullets below stay as the record of what was asked.

**Bench record, 2026-10-18.** The Core's tunnel interface was taken down at 23:22:50: the BPI
went dark at 23:23:40 and opened recovery at 23:24:50, its LAN jack on VLAN 4093 with
`192.168.255.1`, and a laptop on that jack leased `192.168.255.123` from it. Over link-local
(a bench rule, removed after) the page read "Satellite s1 — recovery mode", uplink link up, the
Core answering on the cable (its link-local still did; only the tunnel was down), last handshake
3 min ago, both actions present. The link came back at 23:25:26; at 23:25:47 recovery was off and
the AP resumed, the jack back on guest, the laptop back on its guest lease from the Core, every
recovery section gone. Not exercised: the recovery SSID (the BPI has no sticker password), the
Restart and Factory reset buttons (destructive on the bench), the fresh-router banner (needs a
router with no password cabled to the trunk).

- ☐ (original) A fresh router broadcasts nothing until it has checked its uplink port; one that hears no
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

## Fleet updates (D17)

Decided 2026-10-19: older firmware alone never darkens an AP; a payload it cannot read does.

- ☑ `SyncSnapshot.format` = `SYNC_FORMAT` (1), sent in every payload and in `status` as
  `coreFormat`. An AP told of a newer format writes `/etc/startwrt/needs-update` (outside the
  keep list), refuses the payload and goes dark with its tunnel up; the Core marks it
  `needsUpdate` and stops pushing to it. An AP reporting no format reads format 1.
- ☑ Tunnel `update { registry, targetVersion }`: the AP downloads exactly the Core's version from
  the Core's source (`update::install`, sharing the selection and signature check with
  `system.update`) and reports a failure in `status.update`.
- ☑ `satellite.update` (`fleet_update.rs`): queue in the registry, one sequence, contact wait 10
  min, return wait 15 min; a failure before reboot moves on, an AP that restarts on an older
  version or does not come back stops the sequence.
- ☑ `system.update { updateAccessPoints }`: carried across the Core's reboot as a second line of
  `/etc/startwrt/pending-update` (already in the keep list); the sequence starts 3 min after the
  Core boots the target version.
- ☑ UI: "Update required" / "Update available" / "Newer than the Core" badges replace "Older
  than the Core" (ID 603 reused, unreleased), the update state under the version, an Update
  action per AP, Update all in the page header, and the checkbox on the Core's update card.
- ☑ Found while building it: a recovery layout left in an AP's configs by a reboot was never
  removed (the cleanup keyed on the in-memory recovery flag), leaving its access ports on the
  recovery VLAN. `recovery::exit` now removes the layout whenever it is present, and the AP's
  liveness loop calls it once at start.
- ☑ A failed update's note on the Core clears itself once the access point is no longer behind.

**Bench record, 2026-10-19.** Both routers on the D17 build, then the Core on a test build
sending format 2. Within 5 s the AP logged that the Core sends format 2, went dark (Wi-Fi off,
`eth0` down) with its tunnel live, and the Core marked it `needsUpdate` and pushed nothing
(generation 13 never sent). Recovery mode opened at +75 s with the page lead "The main router's
settings need newer firmware…". Update all from the web UI queued it, sent the update, and the
AP's failure ("requested version 1.3.0 not available": it reached Start9's registry through the
Core's update reach) showed under its version. The first run instead marked it done, because the
version already matched: completion now also needs the AP to read the Core's format. An AP
rebooted while in recovery came back in 40 s, removed the leftover recovery layout at start and
went dark again (the needs-update file survives a reboot). A planted
`pending-update` marker with `access-points` on the Core logged "Updated to v1.3.0" and queued
the AP exactly 3 minutes later. Back on format 1, the AP cleared the file, applied generation 14
and resumed 15 s later with Wi-Fi and `eth0` up and no recovery layout. `bench.sh smoke` passed.
Not exercised: a real download and flash of an AP (needs a registry offering a newer image for
the K1 platform), a halt on an AP that does not come back, more than one AP in sequence.

## Packaging, docs, upstream

- ☑ `build/openwrt.diffconfig`: `kmod-nft-bridge` (batch one); the image was built and flashed on
  both bench routers 2026-10-19. `kmod-wireguard` is already there.
- ☐ Start9's AP board needs its own image target when the board exists.
- ☑ `build/stage-files.sh`: the daemon writes the AP guard variant and the gating file under
  `/usr/share/` itself; the keep set gained the role, registry, generation, reach and applied files.
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
  evidence from both routers. The gate and the guard variant need the image with
  `kmod-nft-bridge` (`make start-wrt-image`, hours) on both routers, which the bench has run since
  2026-10-19; the rest runs with the branch binary deployed over SSH.
