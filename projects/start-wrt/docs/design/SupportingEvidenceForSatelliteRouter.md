# Supporting Evidence for Satellite Router Support

Attachment to the StartWRT satellite-router feature request. The issue states the use case, the
proposal and the file-by-file impact; this document carries the evidence behind it — what breaks in
each workaround available today, which alternatives were weighed and why they were set aside, and
the open concerns that a solution has to answer.

Companion to `satellite-router.md` (design), `satellite-router-next-steps.md` (phased status),
`satellite-router-testplan.md` and `satellite-router-hardware-test.md`.

Code references are against `master` as of 2026-09-20.

---

## 1. What breaks with each workaround available today

StartWRT assigns a Security Profile by point of entry, and every enforcement point is local to one
router: `hostapd` per-PSK dynamic VLAN for Wi-Fi, bridge VLAN filtering for Ethernet, one firewall
zone per profile, one routing table per `vlan_tag`. A user who needs coverage beyond one router's
reach has four options, and each one gives up something the product promises.

| Workaround                                     | What actually breaks                                                                                                                                                                                                                                                                                             |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Consumer mesh node or AP behind a LAN port** | Every device behind it collapses into that port's single profile. A guest phone, a child's tablet and an admin laptop become indistinguishable. `docs/src/ethernet.md` already documents this for switches — an AP is the same case with a radio attached. The profile model is not degraded here, it is absent. |
| **A second, independent StartWRT router**      | Two SSIDs, two password sets, two admin UIs, two sets of profiles kept in sync by hand, and a second WAN uplink the user almost never has. Every profile edit must now be made twice, correctly, forever.                                                                                                        |
| **Long Ethernet run to a remote AP**           | Same one-profile-per-port collapse as the first row, plus the cabling. Moves the radio, not the problem.                                                                                                                                                                                                         |
| **Accept the dead zones**                      | The feature the router was bought for does not reach part of the house.                                                                                                                                                                                                                                          |

The distinguishing constraint is **transparency**. Enterprise gear solves this with RADIUS and
802.1X — per-user credentials, a supplicant to configure, and a concept ladder StartWRT deliberately
spares its users. Preserving "just type the password, or just plug in" is what makes this a design
problem rather than a deployment problem.

Nothing outside the router can reproduce a profile, because a profile is not a Wi-Fi feature: it is a
firewall zone, a subnet, a DHCP scope, a VLAN tag, a routing table and an egress policy, generated
together by `profiles.rs`. Extending coverage while preserving profiles necessarily means the routers
cooperate.

---

## 2. Alternatives considered, and why they were set aside

| Alternative                                                                 | Why not                                                                                                                                                                                                                                                                                                                                                                                               |
| --------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **RADIUS / 802.1X with dynamic VLAN**                                       | The textbook answer, and it gives instant network-wide revocation, which this design does not. It also destroys the defining UX: per-user credentials, a supplicant, a RADIUS server to run and secure. Transparency outranks it.                                                                                                                                                                     |
| **L2 extension — bridge the profile VLANs over the tunnel**                 | Would give seamless roaming with no IP change and need no per-router subnets. Rejected for v1: it puts broadcast/multicast domains across a WAN-ish link, makes DHCP a cross-router race, and turns one flaky backhaul into a layer-2 problem for the whole profile. Per-router subnets are the conservative choice, at the cost of concern 7.                                                        |
| **One trunk tunnel carrying all profiles, separated by VLAN tag inside it** | Fewer tunnels and fewer UDP ports. Rejected because it gives up the property that makes the design cheap: a per-profile tunnel interface simply _joins the profile's firewall zone_ and every existing rule applies unchanged (`FirewallZone.network: Vec<String>`, `uciedit/src/openwrt.rs:24`). A trunk needs new per-tag firewall and routing logic on ingress.                                    |
| **Full config replication — push a backup or raw UCI**                      | Simplest to write and wrong: it clobbers satellite-local identity (hostname, LAN address, certificates, admin credentials, WireGuard keys, role), makes every satellite a potential authority on profile state, and turns an ISP renumbering into a config-sync problem. Pushing semantics keeps the satellite a pure follower.                                                                       |
| **Runtime role switching instead of role-at-flash**                         | Attractive for support, but every Core-only path would have to be correct in both directions at runtime, and a mis-toggled Core silently stops being the source of truth. Reflash is blunt and fails safe.                                                                                                                                                                                            |
| **Dumb APs behind a port (status quo)**                                     | Named deliberately: it works today and costs nothing to build. Its failure is exactly the collapse in §1.                                                                                                                                                                                                                                                                                             |
| **Wi-Fi backhaul in v1**                                                    | Security-neutral — the medium is not the trust boundary, the tunnel is. Deferred from v1 on throughput/reliability grounds and an unresolved bootstrap question (which credential the satellite associates with). See concern 1: on current two-port hardware this is less optional than it first appeared.                                                                                           |
| **StartTunnel as the transport**                                            | Not weighed in the original design, and it should have been. StartTunnel is a WireGuard hub with subnets, per-subnet DNS and egress, a device registry, a PCP/UPnP gateway, and peer authorization by tunnel address plus public key. A satellite is structurally a StartTunnel gateway with the WAN reversed. See `satellite-router-starttunnel-overlap.md` for what carries over and what does not. |

---

## 3. Concerns and open questions

Ordered roughly by blast radius. Concerns 1, 2 and 4 are not implementation details — a wrong answer
changes what gets built.

### Hardware and physical constraints

**1. Port count, and where the backhaul goes.**
`docs/src/hardware.md` specifies the current board as 1 × gigabit WAN and 1 × gigabit LAN. Future
hardware is expected to carry four or more LAN ports, but the design must work on what exists.

The planned test topology resolves most of this: **the satellite uses its WAN port as the transit
uplink** (Core WAN → Internet, Core LAN → Satellite WAN, Satellite LAN → a client, everything else
over Wi-Fi). A satellite has no Internet WAN by definition, so its WAN port is free, and using it for
the backhaul keeps the satellite's single LAN port available for a client.

What that leaves open:

- **`ethernet.rs:45` models `wan_port: Option<String>` as the one port that cannot carry a Security
  Profile.** On a satellite that port is not an uplink, it is the transit link. The role gate has to
  repurpose it rather than special-case it, and the Ethernet page has to show it as something other
  than "WAN."
- **The Core still spends its only LAN port on the transit link.** On today's hardware a Core with a
  satellite attached has no wired point of entry left, so all Core clients are Wi-Fi. Acceptable for
  testing; it needs to be stated plainly as a limitation until four-port hardware ships.
- **Does a multi-satellite deployment on one-LAN-port hardware require a switch?** Hub-and-spoke with
  two satellites needs two transit links from one Core LAN port. That is a switch, and if the transit
  links must be separated it is a _managed_ switch with a VLAN trunk — which StartWRT has no concept
  of today (a port maps to exactly one profile).
- **Wi-Fi backhaul may therefore be load-bearing rather than deferred**, at least for the
  second-and-subsequent satellite. The radio is 4T4R dual-band, so one band could carry backhaul
  while the other serves clients, at the cost of that band. This needs an explicit decision.

**2. Performance ceiling and where the crypto lands.**
All satellite traffic is software-encrypted at both ends on an 8-core RISC-V SoC with 4 GB RAM. The
Core is the concentrator: it terminates every satellite's tunnels, serves its own clients, and for a
VPN-chained profile re-encrypts outbound to the provider — so a satellite client on a chained profile
costs the Core one decrypt plus one or two encrypts. What is the measured throughput per satellite
and in aggregate, and at what point does adding a satellite degrade the Core's own clients? This is a
benchmark, not an estimate, and it bounds how many satellites the product can claim to support.

**3. Tunnel MTU and MSS clamping.**
There is no MSS clamp on the tunnel ingress path and no `mtu_fix` on the profile zones, so large TCP
flows from satellite hosts would black-hole while pings succeed — the failure that presents as "some
websites don't load." Set a correct tunnel MTU and/or `mtu_fix` (reuse the pattern in
`ensure_vpn_outbound_zone`), and put a large-payload transfer in the hardware suite, not just a ping.

### Networking correctness

**4. WAN-less egress on the satellite — the top technical risk.**
A satellite has no WAN, but several paths assume one exists: `vpn_client.rs:1341
rewrite_vpn_chain_routes` pins a VPN endpoint `/32` via the target interface assuming a base uplink;
`profiles.rs:2232 rewrite_routing` builds per-VLAN policy tables with `unreachable` kill-switch
fallbacks that assume a WAN default. Each needs a defined WAN-less behavior: pin the Core tunnel
endpoint via the local transit link, re-point DNS at the Core resolver, and decide what the
kill-switch _means_ on a router whose only uplink is a tunnel.

**There is now a documented upstream pattern for exactly this.** `#4006` added
`shared-libs/crates/start-core/policy-routing.md`, which states the rule ladder and its invariants.
The one named `wg-transport` is the satellite's problem solved in another product: _"A tunnel's
encrypted transport packets route by `main`, never by a selection and never into a rejection.
Otherwise a selected tunnel carries its own transport, and a disconnected one can never reconnect."_
A satellite is that case permanently — its transport must ride the transit link via `main` while
everything else goes into the tunnel. Read that document before designing the satellite's ladder; the
same change also made empty gateway tables reject rather than fall through, which is a satellite's
steady state until its tunnel is up.

**5. Subnet allocation, exhaustion and the existing guards.**
`profiles × (1 + satellites)` `/24`s are needed, Core-allocated. `guard_subnet_collision`
(`profiles.rs:1271`) and `validate_profile_block` (`profiles.rs:1240`) see only local config and
enforce a single `/24`/`/16`. Open: the allocation scheme, the supported maximum, what happens when
the user's chosen LAN range cannot accommodate it, and how that error surfaces _before_ the admin
commits to a topology.

**6. VLAN tag consistency across routers.**
The design requires `vlan_tag` be globally identical. What enforces that when a satellite is paired
to a Core whose profiles already exist, and what happens if the satellite was previously paired
elsewhere?

**7. Roaming: a device changes IP when it changes routers.**
With per-router subnets, walking from the kitchen to the garage drops a device's address. Invisible
for most traffic; not for a long SSH session, a video call, a NAS mount or a self-hosted service
session. There is also no 802.11r/k/v fast-transition story. This is a real regression against a
consumer mesh, which does roam seamlessly. Open: acceptable for v1, how documented, and is L2
extension a credible v2.

**8. Wi-Fi channel planning and co-channel interference.**
Multiple routers on one SSID need channel coordination or they fight each other. Note that #3939 has
since landed regulatory-country support (`wifi.get`/`wifi.set` gain `country`, plus a new
`wifi.regulatory` reporting the channels an AP may currently use per band), which gives the Core the
data it would need to coordinate. Open: does the Core plan channels across satellites, or is it left
to the user — and if left to the user, what does the UI tell them? This extends #3466.

**9. DNS and DHCP split.**
DHCP and DNS are proposed as satellite-local, mirroring the per-gateway model, but profiles carry DNS
policy including DoH/SmartDNS selection. Does a satellite run its own resolver enforcing the profile's
DNS policy, or forward to the Core's? The answer decides whether a child profile's DNS filtering is
enforced identically on both routers — which it must be, or the profile does not mean the same thing.
Note that the single-router case is already broken in this direction: #3673 (custom DNS on a
VPN-routed profile resolves outside the tunnel) and #3672 (DNS leaks to the ISP when the WireGuard
config carries no DNS servers). A WAN-less satellite has no WAN to leak _to_, so the satellite path
may behave differently from the Core path for reasons nobody intended.

**10. Clock and certificates on a WAN-less satellite.**
A satellite cannot reach NTP until its tunnel is up. WireGuard tolerates clock skew; the satellite's
own HTTPS certificate, token expiry and log timestamps do not. What is the boot ordering, how does an
admin reach a satellite's local UI, does the Core's CA cover it, and is it reachable by name?

### Security

**11. Pairing bootstrap is the new trust anchor.**
Admin-initiated, single-use, short-lived enrollment code with key-fingerprint confirmation. This is
the moment a rogue device could become a trusted member of the network. Needs a dedicated review:
code entropy and lifetime, what the admin is asked to verify and whether they will actually verify
it, what happens to a half-completed pairing, and rate limiting.

**12. The satellite's management RPC boundary.**
The satellite's config-apply endpoint must be reachable **only** over the authenticated management
tunnel — never from the LAN or the underlay, where it would be a full remote-configuration surface on
an unauthenticated segment. Today `middleware/auth.rs` accepts a session cookie (`:98`), a local
cookie (`:108`) and any loopback peer (`:144`). #3670 plans to replace that stack entirely; satellite
auth should land as a middleware in the `start-core` OR-composition rather than as a fourth branch in
the current one. See `satellite-router-auth-and-3670.md`.

**13. The upstream channel is a new direction of trust.**
The design starts as a pure Core→satellite push, but the device registry and the port-forward relay
require facts and requests flowing satellite→Core. The bounding invariant proposed is: _a satellite
may only report, or request anything about, addresses inside the subnets the Core allocated it._ That
must be enforced in code and reviewed alongside 11 and 12 — it was not in the original threat model.

**14. Automatic port forwarding behind a satellite, and its replaced security check.**
PCP and UPnP are link-scoped: a device on a satellite sends them to the satellite, never to the Core,
whatever is built. So either the satellite relays and the Core authorizes, or the feature does not
work behind a satellite. Two things to settle:

- `port_control.rs:980 arrival_matches` requires the arrival ifindex to equal the interface the
  neighbor table places the claimed source on. **A satellite client appears in neither.** The
  substitute — arrival on satellite X's authenticated tunnel, plus the claimed address falling inside
  a subnet the Core allocated X — is a _replacement_ and must be reviewed as new work. #3667 is
  relevant supporting reasoning: the existing IPv6 pinhole path already trusts "whatever the neighbor
  table says about this MAC," and that is spoofable on-segment, so a tunnel identity is arguably
  stronger evidence than what the Core trusts today.
- **v1 must refuse explicitly.** Left alone the request dies at the Core as `NOT_AUTHORIZED` behind a
  `tracing::debug!`, so a StartOS server behind a satellite would silently fail to open its own ports
  with nothing anywhere explaining why. A PCP error, a UPnP fault and a visible UI note are the
  minimum bar for shipping the feature deferred.

**15. Credential replication and revocation latency.**
Every satellite holds the full password set, in the same plaintext-in-config posture as one router
today, extended to more devices — including ones in a garage or outbuilding where physical access is
easier.

- **Physical theft** exposes the PSKs, the satellite's WireGuard keys and its token. Unpair must
  revoke instantly at the Core, and the guidance must be to rotate Wi-Fi passwords. Is that enough,
  and is it discoverable in the UI at the moment it matters?
- **Revocation is eventually consistent.** A password removed at the Core is still accepted by an
  offline satellite until it re-syncs. The window is bounded — physical proximity to that specific
  satellite, ends at reconnect, grants no more than the profile it always granted — but it is a real
  departure from single-router behavior, where removal is immediate. Does the UI show which
  satellites have acknowledged a security-relevant change, and does it say so when the admin deletes
  a password?

### Deferred but structural

**16. IPv6 cannot be an afterthought, and its mechanism is unproven.**
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

**17. The device registry is shared substrate, not the cost of one feature.**
Four capabilities need satellite→Core device facts: the Devices page showing satellite clients, the
per-device automatic-forwarding toggle, port-forward authorization, and IPv6 published ports. Build it
once, deliberately. #4007 has since established the shape to follow: `devices.list` reports the UCI
static name as `custom_name` while the resolved name is only a placeholder, and `devices.update`
validates it as a hostname label. That is the fact/policy split the registry needs — a satellite
reports observations, the Core owns `custom_name`, reservations and toggles, and they travel down in
the semantic payload.

### Product, lifecycle and support

**18. How does a user acquire and set up a satellite?**
Role is baked at flash. A satellite SKU, or a user flashing a second standard router? What does the
setup wizard ask, and when is it too late to change the answer? Every unit ships with an EEPROM Wi-Fi
password on a sticker mapped to the Admin profile — what happens to a satellite's sticker password,
which must not be an independent point of entry?

**19. Firmware upgrades and version skew.**
Core and satellites will not update atomically. What is the compatibility contract between a Core on
version N and a satellite on N-1? Does the Core refuse to sync to an incompatible satellite, how is
that surfaced, and is there a supported upgrade order?

**20. Backup, restore, factory reset and Core replacement.**
Does the Core's backup include the satellite registry and pairings? After a restore, do existing
satellites still work or must they re-pair? If a Core dies and is replaced, are its satellites
orphaned until manually reset? What does factory-resetting a satellite do, and what is the recovery
path when a satellite cannot reach its Core? #3662 (encrypt backups with the admin password) is being
designed now, and a pairing token plus WireGuard private keys is exactly the material that should
inform it.

**21. Availability: the Core is a hard dependency.**
The Core holds the only WAN, so if it or the tunnel is down every satellite's clients are islanded —
no Internet, no LAN, possibly no DNS. Inherent to the design, not a bug, but it must be stated in the
docs and must fail _legibly_: what does a client experience, and what does the satellite's own UI say?

**22. Diagnostics and support surface.**
A multi-router network doubles what support has to answer. Per-satellite status, last applied
generation, tunnel health, handshake age, which satellite a device is behind, staleness warnings. Is
there one page that answers "is my network healthy," and are satellite events in the activity log?

**23. Naming and mental model.**
"Core" and "Satellite" are internal terms so far. Whatever the user sees needs testing against the
product's existing vocabulary, which deliberately avoids networking jargon. The docs pages for Points
of Entry, Security Profiles, Ethernet and Wi-Fi are all written for one router and need reworking,
not appending.

---

## 4. Scope and threat model

Home and small business. The security bar is "no obvious holes, and at least as strong as a good
prosumer mesh" — the inter-router link being authenticated and encrypted WireGuard is already stronger
than typical consumer mesh backhaul. Explicitly not a nation-state threat model. The residual risks in
concern 15 are documented as accepted trades, not hidden.

**Non-goals**, to bound the discussion:

- Per-device authentication on a shared wired port — assignment stays per-port, and a switch behind a
  port still collapses to one profile.
- Same-subnet seamless roaming (concern 7).
- Daisy-chained satellites — hub-and-spoke only.
- Runtime role changes.

## 5. Prior art

Consumer mesh (eero, Deco, Orbi) solves coverage with no profile concept — every node is a dumb
extension of one flat network, or at best a separate guest SSID. Enterprise (UniFi, Omada, Aruba
Instant) solves coverage _and_ per-device policy, but with a controller, RADIUS/802.1X, per-user
credentials and a supplicant. The gap this sits in is **enterprise-grade policy segmentation with
consumer-grade "just type the password" enrollment**, which is the product, and the reason the naive
answers do not work.

In-house, StartTunnel is the closest relative and is treated separately in
`satellite-router-starttunnel-overlap.md`.
