# Open issues that collide with the satellite-router work

Working note. Six open issues bear on the satellite design without being about it. For each: what it
actually says, what it does to the satellite plan, and what we might do differently because of it.
#3670 (auth) and the StartTunnel convergence issues (#3681/#3682) are large enough to have their own
notes — `satellite-router-auth-and-3670.md` and `satellite-router-starttunnel-overlap.md`.

Verified against `master` as of 2026-09-20.

---

## 1. #3862 — the router does not know its real IPv6 prefix delegation

**What it says.** `wan_prefix` is initialized to a hardcoded `48` and only ever overwritten from the
UCI `reqprefix` value — the size _requested_, never the size _granted_ (`lan.rs:339-355`). On a
default install `reqprefix` is unset, so the router reports `/48` to a user who may hold a `/64`.
Three consequences, from the issue:

1. The delegated size is not discoverable through StartWRT at all — no CLI, RPC or UI field.
2. `lan.ipv6-get`'s `wan_prefix` reports fiction, and it is the only input to the LAN prefix
   validator (`web/.../lan/routes/ipv6/utils.ts:29-50`).
3. Nothing clamps `ip6assign` to what the delegation can satisfy. The shipped LAN default is
   `ip6assign '60'`, and netifd cannot carve a `/60` out of a `/64` — **so a `/64`-delegated user on
   shipped defaults gets no GUA on any interface, silently.**

The issue also notes the value is already parsed elsewhere: `ssl.rs:343-385
read_gua_prefix_assignments()` runs `ubus call network.interface dump` and reads the real mask, used
today only for published-port retargeting.

**Impact on the satellite work.** Concern 16 says PD size is an external ceiling and that satellites
multiply demand from `profiles` to `profiles × (1 + satellites)` — eighteen `/64`s for six profiles
and two satellites. That arithmetic is only meaningful against the _real_ delegation. Today the
router would compare eighteen against a number it made up. Worse, the failure mode is the one #3862
describes: silent absence of IPv6, which on a satellite would present as "satellite clients have no
v6 while Core clients do" — precisely the symptom the design says breaks the driving constraint, but
caused by a pre-existing bug rather than by anything the satellite work did.

**What to do differently.**

- **Treat #3862 as a prerequisite of the IPv6 phase, and say so in the issue.** Not a nice-to-have:
  satellite v6 allocation cannot be validated against a fictional ceiling.
- **Reuse `read_gua_prefix_assignments()`** rather than adding new plumbing, exactly as the issue
  suggests. The Core's satellite allocator needs the same value the published-port retargeter
  already reads.
- **Make the ceiling a first-class check at pairing.** The natural place to surface "your ISP's
  delegation cannot cover another satellite" is the moment the admin pairs one, not after eighteen
  profiles silently have no v6. That is a satellite-specific feature built on #3862's fix.
- Note the operational detail from the issue: applying a changed `ip6assign` needs a full
  `network restart`, not a reload (`profiles.rs:863-870`). A satellite regenerating its own profiles
  from a semantic push will hit this.

---

## 2. #3667 — the neighbor-table trust model already has a known on-segment hijack

**What it says.** IPv6 published-port rules (`pp_*_v6`) are continuously re-pointed by `ipv6_tracker`
to whatever GUA the kernel neighbor table currently maps to a device's MAC. The trust model is
"whatever the neighbor table says about this MAC," which anyone on the victim's L2 segment can spoof.
An attacker sharing the victim's profile can NDP-spoof the victim's MAC onto an attacker-controlled
GUA; reconcile rewrites the pinhole's `dest_ip`, and inbound WAN traffic is projected to the
attacker. Rated **low severity** deliberately — the attacker must already be on the segment, where
NDP spoofing already grants local MITM. What the mechanism adds is _persistence_ (reachability pings
keep the poisoned entry alive) and _WAN reach_.

**Impact on the satellite work.** Concern 14 argues that `port_control.rs:980 arrival_matches` — which
requires the arrival ifindex to equal the interface the neighbor table places the claimed source on —
cannot apply to a satellite client, who appears in neither. The design proposes a substitute: arrival
on satellite X's authenticated tunnel, plus the claimed address falling inside a subnet the Core
allocated X.

#3667 changes the framing of that proposal. The substitute is not a weakening of a strong check; it
is a _replacement of a check with known limits_ by one with different and arguably better properties.
The tunnel is cryptographically authenticated and its peer identity is pinned at pairing. The
neighbor table is not authenticated at all.

**What to do differently.**

- **Cite #3667 when the substitute is reviewed.** It turns concern 14 from "we are giving something
  up" into "we are trading an unauthenticated signal for an authenticated one, in a case where the
  unauthenticated one is unavailable anyway."
- **Do not let that argument stretch too far.** The tunnel proves _which satellite sent it_, not
  _which device behind that satellite_. Within one satellite's segment, the same NDP/ARP spoofing
  #3667 describes still works, and the satellite's own report is what the Core would trust. The
  bounding invariant (concern 13) limits the damage to that satellite's own clients — which it could
  harm anyway as their gateway — but the residual should be written down, not glossed.
- **Consider whether the satellite device registry should carry a confidence signal** — a DHCP lease
  the satellite issued is much stronger evidence than a neighbor-table entry it observed. #3667
  exists because those two were treated alike.

---

## 3. #3672 / #3673 — profile DNS already escapes VPN-routed profiles

**What they say.** Two mechanisms, same outcome.

- **#3673** — when a profile has a _custom DNS override_, `rewrite_dns_forwarding` takes the
  custom-DNS branch first, regardless of the profile's outbound. The profile's dnsmasq forwards to a
  per-profile SmartDNS group on `127.0.0.1#<5300+vlan_tag>`, and SmartDNS resolves upstream **as the
  router**: its config binds listeners to loopback with bare `server <ip>` upstreams, no
  `bind-device`, no outgoing interface, no fwmark. The VPN policy routing is source-subnet based
  (`prr_<iface>`: `src <lan-subnet>/24 lookup <vlan_tag>`), so a locally-originated packet from an
  unbound socket does not match and falls through to the main table — which by construction has no
  route into the tunnel (`defaultroute '0'`, `route_allowed_ips '0'`). Result: every resolution for a
  VPN-routed profile with custom DNS exits the ISP link with the router's WAN IP as source.
- **#3672** — the companion case: DNS leaks to the ISP when the WireGuard config carries no DNS
  servers at all.

**Impact on the satellite work.** Concern 9 asks whether a satellite runs its own resolver enforcing
the profile's DNS policy or forwards to the Core's. These two issues make the question sharper than it
first looked, in two directions:

- **Replicating the current behavior replicates the bug to a second box.** D5 says the satellite
  regenerates its own config through the existing `profiles.rs` chain — which is the chain containing
  this branch. A satellite running per-profile SmartDNS would reproduce #3673 locally.
- **But the satellite cannot reproduce it, because it has no WAN.** #3673's leak depends on falling
  through to the main table and out the ISP link. On a WAN-less satellite the main table has no
  Internet path at all, so the same code path does not leak — it _fails_. Satellite clients on a
  custom-DNS profile would get no resolution rather than an unencrypted one.

That is a genuinely different behavior on the two routers for the same profile, arising from a bug
neither the design nor these issues anticipated interacting.

**What to do differently.**

- **Decide concern 9 explicitly as "where does a satellite's DNS egress," not "which resolver runs
  where."** The resolver location is a detail; the egress path is the property that must match the
  Core's.
- **The safest default is: a satellite's resolver forwards to the Core over the management tunnel**,
  and the Core applies the profile's DNS policy exactly as it does for its own clients. That makes
  the profile mean the same thing by construction, and it means the satellite inherits whatever fix
  #3672/#3673 receive rather than needing its own.
- **Add a DNS parity test to the hardware suite**: same password on Core and satellite, same query,
  compare the observed upstream and the egress path. The design's test plan checks policy and
  reachability but not this.
- If satellite-local SmartDNS is kept for latency reasons, the WAN-less failure mode must be handled
  deliberately rather than discovered.

---

## 4. #3676 — multiple WAN wants the same abstraction the satellite needs

**What it says.** Support more than one WAN uplink with failover and per-profile egress selection.
Single-WAN is hardcoded throughout: `wan.rs:23 WAN_INTERFACE = "wan"` and every setter writing the
literal `wan`/`wan6` sections; `ethernet.rs:38-56 wan_port: Option<String>` modelling exactly one
uplink port; profile egress being either the literal `"wan"` zone or a VPN interface; published-port
firewall rules hardcoding `src: "wan"`. The scope is "replace the `"wan"` constant with a WAN
identity" across all of those, plus mwan3 for failover.

**Impact on the satellite work.** The satellite design needs three of the same four things, from the
other direction:

- Profile egress must stop meaning "the literal `wan` zone" — on a satellite it means "up the
  tunnel." Concern 4's WAN-less egress _is_ the WAN-identity problem with the identity set to none.
- The WireGuard accept rule's source zone is hardcoded `wan` (`vpn_server.rs:2001`) and must become
  the transit zone. #3676 names the same class of hardcoding in published ports
  (`published_ports.rs:1008,1053`; also `port_control.rs:1161,1384`).
- `ethernet.rs:45 wan_port: Option<String>` is the single-uplink model, and the satellite needs that
  port repurposed as a transit link (see concern 1 and the planned test topology, where the
  satellite's WAN port carries the backhaul).

These are the same refactor. Whichever lands second will either fight the first or quietly duplicate
it.

**What to do differently.**

- **Say in the issue that these two features want one WAN-identity abstraction**, and propose that
  whoever lands first generalizes the constant rather than adding a second special case. This is the
  cheapest possible coordination and it costs one sentence.
- **Design `wan_port` as one member of a small enum, not a boolean special case.** A port is WAN,
  transit, or profile-mapped. That shape serves both features; `Option<String>` plus a satellite
  exception serves neither for long.
- **Watch the direction of the dependency.** The satellite work does not need failover or mwan3, only
  the identity refactor. Do not let it get blocked on the larger feature; do avoid emitting a third
  hardcoded `"wan"`.

---

## 5. #3466 — Wi-Fi channel selection, now partly landed

**What it says.** The Wi-Fi settings page renders channel options from hardcoded US-centric arrays
with no notion of radio capability or regulatory domain — missing valid channels (5 GHz UNII-4;
2.4 GHz 12/13/14 outside the US) and offering channels a non-US radio will refuse, because the valid
set is `radio capabilities ∩ active regulatory domain`. Asks for a jurisdiction selector, a
channel-width (`htmode`) selector, backend-derived channel lists, and band-aware field visibility.

**Partly resolved since.** #3939 landed regulatory-country support: `wifi.get`/`wifi.set` gained
`country` (validated against `/lib/firmware/regulatory.db`, written to every `wifi-device`), a new
`wifi.regulatory` returns the codes the database defines **and the channels an AP may currently use
per band**, parsed from `iw phy`, and the settings page gained a country combobox with the channel
dropdowns filtered to what the domain permits. What remains open from #3466 is the channel-width
selector and band-aware visibility.

**Impact on the satellite work.** Concern 8 asks who plans channels when several routers broadcast
one SSID. Two routers on the same channel in the same house is worse than one router — they contend
rather than cover. Before #3939 the Core had no data to plan with; now it does, via
`wifi.regulatory`'s per-band permitted-channel list.

It also raises a correctness point the design does not mention: **the regulatory country is
device-wide and must be consistent across Core and satellites.** A satellite with a different (or
unset, i.e. world-domain) country would transmit on a different legal channel set while advertising
the same SSID. `country` is a natural member of the semantic sync payload, which currently carries
`ssid`, passwords, ports and profile policy.

**What to do differently.**

- **Add `country` to the semantic sync payload** so a satellite cannot sit on the world domain while
  the Core is set correctly. Small, and it prevents a class of "why is 5 GHz down on the satellite"
  support tickets.
- **Scope channel coordination honestly.** Full RRM is out of scope. A first version can be: the Core
  reads each satellite's `wifi.regulatory`, and the Satellites page warns when two routers are on the
  same channel. That is a UI affordance, not an algorithm, and it is buildable in the same phase as
  the Satellites page.
- **Note #3466's remaining scope as adjacent**, not a dependency. Channel width matters for backhaul
  throughput if Wi-Fi backhaul is ever adopted (concern 1), but nothing in v1 blocks on it.

---

## 6. #3662 — backups are plaintext, and satellites add material worth stealing

**What it says.** A StartWRT backup is the raw output of `sysupgrade --create-backup` — a plain gzip
tarball streamed to the browser with no encryption. It contains `/etc/shadow` (the admin password
hash), `/etc/ssl/private/startwrt-{ca,int,server}.key` (the router's CA, intermediate and server TLS
private keys) and `/etc/config/wireless` (the Wi-Fi PSKs in cleartext). The issue proposes encrypting
with the admin password, StartOS-style.

**Impact on the satellite work.** Concern 20 asks whether the Core's backup carries the satellite
registry and pairings. #3662 makes that question urgent in both directions:

- **A Core backup would gain the satellite registry** — each satellite's pinned public key, its
  allocated subnets, and its pairing token. A pairing token in a plaintext tarball is a credential
  for the management RPC channel.
- **A satellite's own backup is worse.** It holds the full replicated password set, its WireGuard
  private keys, and its pairing token — everything needed to impersonate that satellite to the Core,
  in a file the admin may email to themselves.
- **Restore semantics are undefined.** Restoring a Core from an older backup would restore a stale
  registry: satellites paired since are unknown, satellites unpaired since are trusted again. That
  second case is a revocation being _undone_ by a restore.

**What to do differently.**

- **Contribute the satellite material list to #3662 now, while it is being designed.** It is cheaper
  to have the encryption design account for pairing tokens than to add them afterward.
- **Consider excluding the pairing token from backup entirely.** A token that must be re-established
  by re-pairing after a restore is a smaller blast radius than one that round-trips through a
  tarball, and re-pairing is an admin action that already exists. Restoring a Core could deliberately
  invalidate every pairing and require a re-pair, with the UI saying so.
- **Define restore-versus-registry explicitly** rather than inheriting whatever `sysupgrade
--restore-backup` does: does a restored Core trust the satellites in the backup, the satellites
  currently connected, or neither until confirmed?
- **Concern 20's other half — Core replacement — is still open.** If a Core dies, its satellites hold
  pinned keys for a machine that no longer exists. There must be a documented recovery path that does
  not require physically factory-resetting every satellite.

---

## Summary table

| Issue              | Collision                                                                                         | What changes for us                                                                                 |
| ------------------ | ------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| **#3862**          | Real PD size is unknown; `/64` users silently get no GUA                                          | Prerequisite of the IPv6 phase; reuse `read_gua_prefix_assignments()`; check the ceiling at pairing |
| **#3667**          | Neighbor-table trust is already spoofable on-segment                                              | Strengthens concern 14's substitute check; but the tunnel proves the _satellite_, not the device    |
| **#3672/#3673**    | Profile DNS escapes the tunnel on the Core; would _fail_ rather than leak on a WAN-less satellite | Forward satellite DNS to the Core; add a DNS parity test                                            |
| **#3676**          | Multi-WAN needs the same WAN-identity refactor                                                    | Coordinate; model a port as WAN/transit/profile rather than `Option<String>`                        |
| **#3466 (+#3939)** | Channel data now exists via `wifi.regulatory`                                                     | Put `country` in the sync payload; ship a same-channel warning, not an algorithm                    |
| **#3662**          | Backups are plaintext; satellites add tokens and keys                                             | Feed the material list into #3662; consider excluding pairing tokens from backup entirely           |
