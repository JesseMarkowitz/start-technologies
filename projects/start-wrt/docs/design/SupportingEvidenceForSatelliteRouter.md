# Supporting Evidence for Satellite Router Support

Attachment to the StartWRT satellite-router feature request. The issue states the use case and the
proposal in product terms, and names the alternatives and the affected files; this document carries
the reasoning behind them — why each alternative was set aside, the twelve questions that bear on
whether and when this should be built, the implementation concerns surveyed, the scope and threat
model assumed, prior art, and the file-by-file impact in full.

Code references are against `master` as of 2026-09-20.

---

## 1. Alternatives considered, and why they were set aside

1. **RADIUS / 802.1X with dynamic VLAN.** The textbook answer, and it offers something this design
   does not: instant network-wide revocation of a credential. It is set aside because of what it
   asks of the user. Every device needs its own credential and an 802.1X supplicant configured, and
   the household needs a RADIUS server to run and secure. The product's defining promise is that a
   device joins by typing a password and doing nothing else; keeping that promise is worth more than
   the revocation advantage RADIUS would add.

2. **One trunk tunnel carrying all profiles, separated by VLAN tag inside it.** Fewer tunnels and
   fewer UDP ports. Set aside because it gives up the property that makes the design cheap: a
   per-profile tunnel interface simply _joins the profile's firewall zone_ and every existing rule
   applies unchanged (`FirewallZone.network: Vec<String>`, `uciedit/src/openwrt.rs:24`). A trunk
   needs new per-tag firewall and routing logic on ingress.

   The cost of that simplicity is that interface count grows as profiles × satellites, plus one
   management tunnel per satellite. Whether that is affordable at the scale the product actually
   targets is concern 2, not a reason to reject the per-profile shape here; the trunk remains the
   escape hatch if measurement says otherwise.

3. **Full config replication — push a backup or raw UCI.** Simplest to write, but it adds risks.
   It clobbers satellite-local identity (hostname, LAN address, certificates, admin credentials,
   WireGuard keys, role), and it makes every satellite a potential authority on profile state. It
   also turns an ISP renumbering into a config-sync problem: when the ISP hands the Core a new
   address — a new dynamic IPv4 on the WAN, or a new delegated IPv6 prefix — a replicated
   configuration carries those addresses with it, so every satellite has to be re-pushed and
   re-applied before the network is consistent again. A semantic payload contains no Core addresses
   at all, so the same event changes nothing on any satellite. Pushing semantics keeps the satellite
   a pure follower.

4. **Switching role while the router is running.** Attractive because it lets an owner change the
   shape of their network from the UI without touching the hardware, but it adds risks. It makes
   every Core-only behavior reversible mid-flight — profile authorship, WAN setup, remote access,
   schedule and cron regeneration are ongoing work the daemon starts and keeps doing, so each would
   have to stop cleanly and leave behind nothing that contradicts the new role. It also turns an
   accidental change into a silent one: a satellite flipped in place keeps its clients and its
   pairing while beginning to author profiles locally, so two boxes believe they own the profiles
   and the next sync either overwrites the divergence or refuses it. The proposal is instead that a
   role changes only across a reboot and only as a deliberate, destructive act — the router erases
   its previous role's state, resets to factory defaults and comes up blank in the new role — so a
   router converted by mistake is inert rather than divergent. This supersedes the design's D6
   ("chosen at setup, baked at flash; reflash to change").

5. **Dumb APs behind a port.** This is the status quo, rejected directly in the issue. It works
   today and costs nothing to build; its failure is exactly the profile collapse the issue
   describes.

6. **Wi-Fi backhaul — the routers link to each other over Wi-Fi instead of a cable.** The proposal
   connects Core and satellite with an Ethernet cable and runs the per-profile tunnels over it.
   Wi-Fi backhaul changes only that one link: the satellite associates to the Core's radio, and the
   same tunnels run over the wireless hop. Nothing else about the design changes, and it is
   security-neutral — the medium is not the trust boundary, the encrypted tunnel is.

   **Throughput and reliability.** A backhaul sharing a radio and channel with the clients it serves
   carries every packet over the air twice — once from the client to the satellite, once from the
   satellite to the Core — so the airtime available to clients is roughly halved, and the loss
   compounds as more satellites share the same air. A cable's capacity is fixed and known; a radio
   link's varies with distance, walls, neighbouring networks and interference, which are exactly the
   conditions that made the user add a satellite in the first place. That makes every profile's path
   to the internet depend on radio conditions rather than on a wire.

   **The bootstrap problem.** A cable works the moment it is plugged in: the satellite can reach the
   Core and be paired before any credential exists. Wi-Fi cannot — to talk to the Core at all, the
   satellite must first associate to its radio, and associating requires a password. Which one is
   unresolved. If it uses a profile's password, the satellite spends its unpaired life as an
   ordinary client inside a user profile, holding the very credential it is supposed to be enforcing.
   If the Core broadcasts a dedicated backhaul network for it, that is a new always-on credential to
   protect, present even when no satellite is being paired. The link needed to pair is gated by a
   credential that pairing is meant to establish.

   Set aside on those grounds rather than on principle. It is worth revisiting as an optional
   addition later, because it frees a LAN port on both boxes — which matters given the limited
   availability of LAN ports (concern 1).

7. **StartTunnel as the transport.** StartTunnel is Start9's VPS-hosted virtual private router:
   remote segments dial into it over WireGuard and it forwards clearnet traffic to them. A satellite
   is structurally a StartTunnel spoke with the WAN reversed, so the question is whether StartWRT
   should route satellites through it rather than build a site-to-site path of its own.

   Set aside, because a StartTunnel spoke is a _host_ holding one tunnel address —
   `WgSubnetConfig.clients` is a `BTreeMap<Ipv4Addr, WgConfig>`, one entry per client
   (`start-core/src/tunnel/wg.rs:116`) — whereas a satellite is a _router_ that must advertise a
   whole subnet on behalf of clients it serves locally. StartTunnel also keeps its state in PatchDB
   and serves its own JSON-RPC API, so adopting it as the transport would introduce a second state
   model onto a box whose configuration is UCI.

   Setting it aside as _the transport_ is not the same as ignoring it: several of its parts are
   directly reusable, which is concern 7.

---

## 2. Questions that bear on the decision

Twelve questions whose answers could change whether, when or how this is built. Everything else that
was surveyed is named in §3.

**1. Port count, and where the backhaul goes.**
The initial release of the StartWRT router has one WAN port and one LAN port; future hardware is
expected to carry four or more LAN ports. The topology that follows is a Core connected to the
Internet, serving its own clients over Wi-Fi and spending its single LAN port on the backhaul to the
satellite; the satellite using its WAN port for that backhaul — it has no Internet uplink of its
own, so that port is free — and keeping its LAN port for local connections, perhaps through a
switch, on which every device appears in one profile.

The consequence is a hard limit on today's hardware: **until StartWRT routers ship with more
Ethernet ports, a two- or three-satellite deployment is not possible**, because each satellite needs
its own wired connection back to the Core. One Core and one satellite is what the current boards
support. A reasonable person could argue from that alone that the feature should wait for
multi-port hardware; the counter-argument is that one satellite already buys coverage where there is
none today.

It also means a satellite's WAN port is a transit link rather than an uplink, while `ethernet.rs:45`
models `wan_port` as the one port that cannot carry a Security Profile — the role has to repurpose
that port rather than special-case it.

**2. Performance ceiling, and where the crypto lands.**
All satellite traffic is software-encrypted at both ends on an 8-core RISC-V SoC with 4 GB RAM. The
Core is the concentrator: it terminates every satellite's tunnels, serves its own clients, and for a
VPN-chained profile re-encrypts outbound to the provider — so a satellite client on a chained
profile costs the Core one decrypt plus one or two encrypts.

Testing should be done to benchmark this and establish how many satellites and profiles a board of
the current size can comfortably support. Nothing has been measured, so no range is claimed here. If
satellites materially degrade the Core's own clients — or if supporting them would need more memory
or more cores than this board has — that bears on whether the feature belongs on this hardware
generation at all.

**3. DNS and DHCP must behave identically on both routers.**
DHCP and DNS are proposed as satellite-local, but profiles carry DNS policy including DoH and
SmartDNS selection. Whether a satellite runs its own resolver enforcing that policy or forwards to
the Core's decides whether a child profile's DNS filtering is enforced identically on both routers —
and it has to be, or the profile does not mean the same thing in the garage as in the kitchen.

It is not acceptable for the satellite path to behave differently from the Core path. Both must be
deterministic, and both must fail safely rather than in some unexpected direction. The single-router
case is already broken in this direction — #3673 (custom DNS on a VPN-routed profile resolves
outside the tunnel) and #3672 (DNS leaks to the ISP when the WireGuard config carries no DNS
servers) — and a WAN-less satellite has no WAN to leak _to_, so the two paths may diverge for
reasons nobody intended.

**4. Cross-router service discovery.**
mDNS is link-local by construction — multicast to `224.0.0.251` / `ff02::fb` with IP TTL 1, and
RFC 6762 requires receivers to enforce that scope — so no router forwards it. With a separate subnet
per router, a device on a satellite cannot discover or resolve a device on the Core **within the
same profile**. Discovery across profiles is already blocked deliberately; what is new is that one
profile stops being one discovery domain, which is the invariant this feature exists to preserve.

It lands hardest on Start9's own product. A StartOS server is reached on the LAN at
`<hostname>.local` and resolves it through avahi, because its musl-linked binaries implement no NSS
(`start-core/src/net/mdns.rs`). The same applies to AirPrint/IPP printers, Chromecast and AirPlay,
HomeKit, and the broadcast-based setup flows most IoT devices use.

Three candidate mechanisms, not mutually exclusive:

- **Bridge each profile's segment across the routers (layer-2 extension).** The complete answer:
  every link-local protocol works, including those a reflector cannot carry (SSDP/DLNA,
  WS-Discovery, NetBIOS), and a device keeps its address as it moves. WireGuard carries IP only, so
  this needs VXLAN or GRETAP inside the tunnel, and neither kmod is in the image today
  (`build/openwrt.diffconfig` ships `bridge` and `ip-bridge`). It costs a client-visible MTU of
  roughly 1390 on an IPv4 underlay, replicates broadcast and multicast to every satellite, makes
  DHCP dependent on the backhaul, and creates a layer-2 loop if a second path is ever cabled between
  the boxes. Its CPU cost on this hardware is unmeasured — see concern 2.
- **Relay multicast between the routers (an mDNS reflector).** Much lighter: no encapsulation, no
  MTU change, the routed design intact, and `avahi-dbus-daemon` already ships in the image with a
  reflector mode. It has to be scoped per profile or it destroys profile isolation, two reflectors
  can loop, and it carries mDNS only — Windows/SMB discovery and SSDP still fail.
- **Publish names over unicast DNS instead of discovering them (DNS injection).** Already built in
  this codebase for the same reason: `spawn_server_mdns_injection`
  (`start-core/src/net/dns_update/mod.rs:210`) pushes a StartOS box's `<hostname>.local` to its
  gateway over RFC 2136 precisely because clients on a tunnel cannot do mDNS. StartWRT does not
  implement the receiving side today. It solves the StartOS case exactly and does nothing for
  printers or IoT.

The problem is certain; the mechanism is open. Whichever is chosen has to preserve the boundary:
discovery may cross routers, never profiles.

**5. StartOS services behind a satellite.**
At least initially, nearly every StartWRT owner is also running one or more StartOS servers, and
those servers publish services. **The environment has to keep working when a StartOS server, or a
client of one, sits behind a satellite** — services reachable by mDNS name, by LAN address, and
through gateways including StartTunnel and Tor. If the design cannot support that, it is not
practical to proceed with it.

Automatic port forwarding is the part that does not survive the move unchanged. PCP and UPnP are
link-scoped: a device on a satellite sends them to the satellite, never to the Core. So either the
satellite relays and the Core authorizes, or the feature does not work behind a satellite. Two
things to settle:

- `port_control.rs:980 arrival_matches` requires the arrival ifindex to equal the interface the
  neighbor table places the claimed source on, and **a satellite client appears in neither**. The
  substitute — arrival on satellite X's authenticated tunnel, plus the claimed address falling
  inside a subnet the Core allocated X — is a _replacement_ and must be reviewed as new work. #3667
  is supporting reasoning: the existing IPv6 pinhole path already trusts whatever the neighbor table
  says about a MAC, which is spoofable on-segment, so a tunnel identity is arguably stronger
  evidence than what the Core trusts today.
- **Until the relay exists, the refusal must be explicit.** Left alone the request dies at the Core
  as `NOT_AUTHORIZED` behind a `tracing::debug!`, so a StartOS server behind a satellite would
  silently fail to open its own ports with nothing anywhere explaining why. A PCP error, a UPnP
  fault and a visible UI note are the minimum bar.

**6. Credential replication, revocation latency, and the physical-access model.**
Every satellite holds the full password set, in the same plaintext-in-config posture as one router
today, extended to more boxes — including ones in a garage or outbuilding where physical access is
easier. The security model should be stated plainly in the documentation: **physical access to a
router implies full control of it**, including its keys and stored credentials. Because a device's
profile is decided by the port it is plugged into, physical access to the cabling is part of that
boundary. This is not a design for high-security business environments; where more is needed, the
router and its cabling have to be physically secured.

Two consequences follow:

- **Theft of a satellite** exposes the PSKs, its WireGuard keys and its token. Unpair must revoke
  instantly at the Core, and the guidance has to be to rotate Wi-Fi passwords — discoverably, at the
  moment it matters.
- **Revocation is eventually consistent, and that needs a bound.** A password removed at the Core is
  still accepted by an offline satellite until it re-syncs, so someone disabled or moved to a
  lower-privileged profile keeps their old access on that satellite in the meantime. There should be
  a time-limited window within which a satellite must acknowledge a security-relevant change, with a
  defined consequence when it does not. The UI should show which satellites have acknowledged.

**7. Code that should be shared with StartTunnel rather than written twice.**
StartTunnel is not the transport (§1, alternative 7), but it is the same crate StartWRT already
depends on — `port_control.rs:128` already imports `startos::tunnel::forward::sni` — and several
pieces of it are directly reusable: WireGuard key and PSK handling (#3681 already names this as a
lift into `shared-libs/`), per-segment policy as a data shape (`WgSubnetConfig` at
`start-core/src/tunnel/wg.rs:116` carries a segment's own resolver and egress), routed IPv6 over a
WireGuard link with no delegation protocol (`tunnel/wg6.rs`), and authorizing a peer by tunnel
address and public key, which #3682 already proposes and concern 5 also needs. Worth deciding
jointly with #3681 and #3682 rather than building in isolation.

**8. How a satellite is acquired and set up.**
Role is chosen at initial setup, and changing it later is a factory reset and reboot into the new
role. That has a product consequence beyond the code: is there a satellite SKU, or does a user buy a
second standard router and convert it? What does the setup wizard ask, and how is the choice
presented to someone who does not yet know what a satellite is? Every unit ships with an EEPROM
Wi-Fi password on a sticker mapped to the Admin profile — what becomes of a satellite's sticker
password, which must not be an independent point of entry?

**9. Backup, restore and Core replacement.**
A satellite holds no authoritative state, which makes the shape of this simple: the Core is backed
up, satellites are not, and a failed satellite is replaced, comes up blank and re-syncs. If a
restored Core is recent enough to be in step with its satellites, they should reconnect as they
would after a power cut. If the backup is older than what the satellites have, the Core is still
authoritative — the changes made after that backup are lost, and a satellite must not be able to
rejoin with newer state; it is reset and re-paired. #3662 (encrypt backups with the admin password)
is being designed now, and a pairing token plus WireGuard private keys is exactly the material that
should inform it.

**10. Availability: the Core is a hard dependency.**
The Core holds the only WAN, so if the **Core** is down every satellite's clients are islanded. That
appears to be inherent, and the working answer is yes — losing the Core means losing the network —
but it is a decision that deserves to be made deliberately rather than inherited. Losing the
**WAN**, by contrast, must not do that: with the Core up and the Internet down, every client on
every router must still reach every other, exactly as a single router behaves today. Whatever the
answer, the failure has to be legible — what the client experiences, and what a satellite's own UI
says.

**11. Diagnostics, and reaching a satellite that cannot reach its Core.**
Satellite events should travel up to the Core so it holds the master record, and a satellite should
also log locally so there is something to read when it cannot reach the Core. The sharp case is
administration itself: if authentication and profile data come from the Core, a satellite that
cannot reach it may also be the satellite nobody can log into — exactly when someone needs to. How a
satellite is reached while booting, while unpaired, and while failing needs to be designed, not
discovered.

**12. Naming and mental model.**
"Core" and "Satellite" are internal terms so far. Whatever the user sees needs testing against the
product's existing vocabulary, which deliberately avoids networking jargon, and the documentation
pages for Points of Entry, Security Profiles, Ethernet and Wi-Fi are all written for one router and
would need reworking rather than appending. Worth settling before a public release.

---

## 3. Implementation concerns surveyed

Real work with known approaches. None of these bears on whether the feature should be built, so they
are named here rather than argued.

- **Tunnel MTU and MSS clamping.** No MSS clamp on tunnel ingress and no `mtu_fix` on profile zones
  today, so large TCP flows would black-hole while pings succeed.
- **WAN-less egress on the satellite.** The largest implementation risk: several paths assume a WAN
  exists. #4006 documented the policy-routing rule ladder, and its `wg-transport` invariant is this
  problem already solved in another product.
- **Subnet allocation and the existing guards.** `profiles × (1 + satellites)` `/24`s, Core-allocated;
  `guard_subnet_collision` and `validate_profile_block` see only local config today.
- **Roaming: a device changes IP when it changes routers.** Invisible for most traffic, not for a
  long-lived session. Layer-2 extension (concern 4) would remove it entirely.
- **Wi-Fi channel planning across routers.** #3939 landed the regulatory data that makes coordination
  possible; whether the Core plans channels or the user does is a UI decision. Extends #3466.
- **Clock and certificates on a WAN-less satellite.** No NTP until the tunnel is up; WireGuard
  tolerates skew, certificates and token expiry do not.
- **Pairing bootstrap.** Code entropy and lifetime, what the admin verifies, half-completed pairings,
  rate limiting.
- **The satellite's management RPC boundary.** The config-apply endpoint must be reachable only over
  the authenticated management tunnel, and should land on #3670's model rather than beside it.
- **The upstream channel as a new direction of trust.** Bounding invariant: a satellite may only
  report, or request anything about, addresses inside the subnets the Core allocated it.
- **IPv6.** Prefix delegation over a WireGuard interface, the routed-prefix alternative already in
  `tunnel/wg6.rs`, and the external ceiling #3862 describes.
- **The device registry.** Shared substrate for the Devices page, per-device toggles, port-forward
  authorization and IPv6 published ports; #4007 established the fact/policy split to follow.
- **Firmware upgrades and version skew.** The compatibility contract between a Core on version N and
  a satellite on N-1, and the supported upgrade order.
- **Changing a router's role safely.** Complete erase, a backup prompt when converting a Core,
  re-authentication at the moment of conversion, and atomicity across a power loss.

---

## 4. Scope and threat model

Home and small business. The security bar is "no obvious holes, and at least as strong as a good
prosumer mesh" — the inter-router link being authenticated and encrypted WireGuard is already stronger
than typical consumer mesh backhaul. Explicitly not a nation-state threat model. The residual risks in
concern 6 are documented as accepted trades, not hidden.

**Non-goals**, to bound the discussion:

- Per-device authentication on a shared wired port — assignment stays per-port, and a switch behind a
  port still collapses to one profile.
- Same-subnet seamless roaming (§3; concern 4 would remove it).
- Daisy-chained satellites — hub-and-spoke only.
- Runtime role changes.

## 5. Prior art

Consumer mesh (eero, Deco, Orbi) solves coverage with no profile concept — every node is a dumb
extension of one flat network, or at best a separate guest SSID. Enterprise (UniFi, Omada, Aruba
Instant) solves coverage _and_ per-device policy, but with a controller, RADIUS/802.1X, per-user
credentials and a supplicant. The gap this sits in is **enterprise-grade policy segmentation with
consumer-grade "just type the password" enrollment**, which is the product, and the reason the naive
answers do not work.

In-house, StartTunnel is the closest relative — see #3682 and #3681, and §1 above.

---

## 6. File-by-file impact

Code references are against `master` as of 2026-09-20. The issue states this at module level; this is
the detail behind it.

| File                               | Change                                                                                                                                                                                                                                                 |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `ctrl/src/vpn_site.rs` (new)       | site-to-site WG: subnet-advertising peer, transit underlay, per-profile bring-up, WAN-less endpoint pinning, skip proxy-ARP (`vpn_server.rs:802 sync_proxy_arp` assumes on-link hosts)                                                                 |
| `ctrl/src/satellite.rs` (new)      | role state, pairing, registry, config-sync RPC                                                                                                                                                                                                         |
| `ctrl/src/profiles.rs`             | attach a remote `/24` to the zone + per-VLAN table; teach `guard_subnet_collision:1271` / `validate_profile_block:1240` about Core-allocated satellite subnets; MSS clamp on ingress; `rewrite_routing:2232`'s kill-switch `unreachable` assumes a WAN |
| `ctrl/src/vpn_server.rs`           | parameterize the accept rule's source zone (`:2001 src: "wan"`); factor WG helpers. Do not overload the `/32` host-peer path                                                                                                                           |
| `ctrl/src/vpn_client.rs`           | `rewrite_vpn_chain_routes:1341` pins an endpoint `/32` assuming a base uplink exists                                                                                                                                                                   |
| `ctrl/src/middleware/auth.rs`      | remote-peer auth. Today: session cookie, local cookie, loopback bypass at `:144`. Should land on #3670's model, not beside it                                                                                                                          |
| `ctrl/src/bins/daemon.rs`          | role gate around the Core-only block in `inner_main:201`                                                                                                                                                                                               |
| `ctrl/src/setup.rs`, `ethernet.rs` | role at setup; transit port on a satellite. `ethernet.rs:45` models `wan_port` as the one port that cannot carry a profile, so a satellite needs it repurposed as transit rather than uplink                                                           |
| `ctrl/src/devices.rs`              | enumerates `ip neigh` (`:207`) and dnsmasq leases (`:240`), both strictly local — needs a satellite-reported read path                                                                                                                                 |
| `ctrl/src/port_control.rs`         | `arrival_matches:980` cannot apply to a routed client; v1 refuses PCP/UPnP explicitly rather than failing silently                                                                                                                                     |
| `ctrl/src/system.rs`               | role conversion reuses the existing factory reset (`:742`, OpenWrt `firstboot`) rather than a second eraser                                                                                                                                            |
| `web/`                             | Satellites page, pair dialog, staleness indicator; `api.service` trio                                                                                                                                                                                  |

**Why the firewall side is cheap.** A per-profile tunnel interface simply joins the profile's
`vlan_<iface>` zone, because zone membership is keyed on ingress interface
(`FirewallZone.network: Vec<String>`, `uciedit/src/openwrt.rs:24`; `profiles.rs:1828
rewrite_firewall`), not on subnet. Existing WAN access, LAN access, cross-profile forwarding and VPN
chaining then apply to a satellite's clients with no new firewall logic — which is also why the
single-trunk alternative in §1 was rejected.
