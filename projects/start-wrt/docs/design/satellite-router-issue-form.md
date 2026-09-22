# Issue form — as submitted

**Posted 2026-09-21 as `Start9Labs/start-technologies#4043`**
(https://github.com/Start9Labs/start-technologies/issues/4043), label `StartWRT`. The two closing
questions were posted as the first comment; `SupportingEvidenceForSatelliteRouter.md` was uploaded
as the second. This file is the record of what was submitted.

Field-by-field text for `Start9Labs/start-technologies` → New issue → **💡 Feature Request**
(`.github/ISSUE_TEMPLATE/9-feature-request.yml`). Paste each block into the field of the same name.
Attach `SupportingEvidenceForSatelliteRouter.md` to the issue after creating it.

Technical source: `SupportingEvidenceForSatelliteRouter.md`.

---

## Prerequisites (checkbox)

☑ I searched existing issues and this is not already requested.

Verified 2026-09-21: no open or closed issue in this repo requests multi-router coverage, mesh,
access-point, repeater or roaming behaviour for StartWRT.

## Project (dropdown)

**StartWRT**

## Title

StartWRT: extend Security Profiles across multiple routers (Core + Satellite)

---

## Problem & use case

Security Profiles work because a device's profile is decided by _how it joins_: the Wi-Fi password it used, or the port it plugged into. Type the guest password, you are on Guest. Plug into the port assigned to Kids, you are on Kids. Nothing is installed on the device and nothing is configured by the person using it.

That decision is made and enforced entirely inside one box — the radio that accepted the password, the port that saw the cable, and the firewall and routing rules that then apply. **Profile coverage therefore stops where that one router's radio and ports stop.**

Two ordinary situations that break this model:

- **More area than one antenna covers.** A larger house, masonry walls, a second floor, a detached garage or workshop. Devices out there cannot reach the router.
- **More wired devices than the router has ports.** The initial device available today has a single LAN port. The second-generation router is expected to ship with only four LAN ports — a single office with a workstation, printer, IoT hub and NAS consumes all four, with no room for anything else.

In both cases the owner reaches for the obvious fix — an access point, a switch, a second router — and in doing so loses access to the profile model, the flagship advantage of this router.

**What I am trying to get to**

- **One SSID.** The same network name everywhere in the building.
- **One set of profiles.** The same password means the same thing on any router.
- **No client-side configuration.** No certificates, no per-device logins, no supplicant. Type the password.
- **One place to administer.** Profiles are created and edited once, in one UI.

**Why today's options do not get there**

- **A mesh node or access point behind a LAN port.** Every device behind it inherits that one port's profile. A guest's phone, a child's tablet and an admin laptop all land in the same place and the router cannot tell them apart. The profile model is not degraded here, it is absent.
- **A switch behind a LAN port.** The same collapse for wired devices — everything on the switch shares one profile, as StartWRT's own Ethernet documentation already states. A long cable to a remote access point is the same case again: it moves the radio, not the problem.
- **A second, independent StartWRT router.** Two SSIDs, two sets of passwords, two admin UIs, and two copies of every profile to keep identical by hand, forever — plus a second internet uplink the owner almost certainly does not have.

Enterprise equipment solves coverage _and_ per-device policy together, but it does it with RADIUS and 802.1X: per-user credentials, a supplicant configured on every device, and a server to run and secure. StartWRT was designed precisely to make it easier for people to manage, so they do not need to implement that type of solution.

**Concretely:** I need this in my own house, where one router cannot cover the space. Two other people I know would deploy it as soon as it existed. If Start9's direction includes business users, the same need shows up there first and hardest.

---

## Proposed solution

Let a second or third StartWRT router act as an extension of the first, instead of as a network of its own.

One router is the **Core**. It holds the internet connection, owns the profiles, and is the only place they are edited. The others are **Satellites**: no internet connection of their own and no authority over profiles — they extend the Core's network into the parts of the building it cannot reach. The role is chosen at setup, and changing it later resets the router to factory defaults and reboots it into the new role — never a switch in place, so there is no half-converted router and no second box that thinks it owns the profiles.

What the StartWRT owner does: configure the second router as a Satellite, connect it to the Core, and pair it from the Core's UI using a one-time code. It comes up broadcasting the same SSID with the same passwords. A phone that joins it with the guest password is on Guest — same isolation, same rules, same internet policy — and appears in the same device list.

Four ideas make that work:

1. **The profile is still decided at the point of entry.** A Satellite runs the same per-password and per-port logic the Core does, so the decision happens locally the moment a device joins. Nothing is asked of the device, and no lookup elsewhere has to succeed first.
2. **Each profile gets its own encrypted tunnel between the routers.** One WireGuard tunnel per profile per Satellite, plus one for management. Traffic that entered on Guest stays in the Guest tunnel for the whole trip to the Core, so the separation survives the hop between boxes. The tunnel count depends on how many profiles and Satellites exist, not on how many devices are connected.
3. **The Core stays the only way out, and the only place policy is applied.** A Satellite keeps profiles apart locally and sends everything else upstream; the Core applies the profile's rules and provides internet access. One enforcement point, not two that have to be kept in agreement.
4. **The Core sends the profiles themselves, not a copy of its own settings.** A Satellite never receives the Core's configuration. It gets a short description — which profiles exist, which passwords and ports lead to them, and what each one is allowed to do — and builds its own local network settings from that. Every router keeps its own identity, and only the Core can change what a profile means. Each update is numbered, so a Satellite can reject an out-of-date one.

Pairing is deliberately explicit: an admin-initiated, single-use enrollment code, with the Satellite's key pinned on acceptance. Unpairing revokes it at the Core.

**Hardware constraint.** Initially, all routers will have only one WAN port and one LAN port. The intended layout is Core WAN to the internet, Core LAN to the Satellite's **WAN** port, and a client on the Satellite's LAN port, with everything else over Wi-Fi. Using the Satellite's WAN port for the link to the Core keeps its LAN port free for a device — which means a Satellite needs that port repurposed as an inter-router link rather than an uplink. It also caps today's hardware at one Core and one satellite, since each satellite needs its own wired connection back.

**Bounded on purpose.** Hub-and-spoke only, no daisy-chained satellites, no runtime role switching, and a home / small-business threat model. Non-goals are listed in §4 of the attachment.

**Work this implies**

- [ ] Role + provisioning; daemon gate
- [ ] Site-to-site transport + WAN-less egress
- [ ] Routed attachment at the Core
- [ ] Cross-router service discovery
- [ ] Pairing + remote auth — security review; reconcile with #3670
- [ ] Config sync
- [ ] Capability gating + UI
- [ ] Explicit PCP/UPnP refusal on a satellite
- [ ] Device registry (first satellite→Core direction)
- [ ] IPv6
- [ ] Automatic port-forward relay

---

## Alternatives considered

Seven were weighed. The reasoning for each is in §1 of the attached `SupportingEvidenceForSatelliteRouter.md`.

1. **RADIUS / 802.1X with dynamic VLAN.** The textbook answer, and it adds instant network-wide revocation. Set aside because it requires per-user credentials, a supplicant on every device and a server to run — the concept ladder StartWRT exists to avoid.
2. **One trunk tunnel carrying all profiles, separated by VLAN tag inside it.** Fewer tunnels and ports, but it gives up the property that makes this cheap: a per-profile tunnel interface simply joins the profile's existing firewall zone and every current rule applies unchanged. A trunk needs new per-tag firewall and routing logic on ingress.
3. **Full config replication — push a backup or raw UCI.** Simplest to write, but it clobbers satellite-local identity, makes every satellite a potential authority on profile state, and turns an ISP renumbering into a config-sync problem.
4. **Switching role while the router is running.** Easier for an owner, but it makes every Core-only behaviour reversible mid-flight and turns an accidental change into a silent one — two boxes each believing they own the profiles. Hence role changes only across a reboot, as a deliberate reset.
5. **Dumb APs behind a port.** The status quo. Works today, costs nothing, and collapses every device behind it into one profile.
6. **Wi-Fi backhaul instead of a cable.** Security-neutral, but it halves client airtime on a shared radio and has an unresolved bootstrap problem: associating requires a credential that pairing is meant to establish. Worth revisiting later — it frees a LAN port on both boxes.
7. **StartTunnel as the transport.** A satellite is structurally a StartTunnel spoke with the WAN reversed, but a StartTunnel spoke is a _host_ holding one tunnel address, while a satellite is a _router_ advertising a subnet — and its state lives in PatchDB rather than UCI. Set aside as the transport; several of its parts are still worth reusing (see below).

**Layer-2 extension** — bridging each profile's segment across the routers — is deliberately _not_ in this list. It is a live option rather than a rejected one, and it sits in the attachment as concern 4 (cross-router service discovery), because mDNS does not cross a routed boundary and that affects reaching a StartOS server by `.local`, printers and IoT discovery.

---

## Affected areas

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
| `ctrl/src/port_control.rs`         | `arrival_matches:980` cannot apply to a routed client; refuse PCP/UPnP explicitly rather than failing silently until a relay exists                                                                                                                    |
| `ctrl/src/system.rs`               | role conversion reuses the existing factory reset (`:742`, OpenWrt `firstboot`) rather than a second eraser                                                                                                                                            |
| `web/`                             | Satellites page, pair dialog, staleness indicator; `api.service` trio                                                                                                                                                                                  |

Line references are against `master` as of 2026-09-20.

---

## Anything else?

**Some of this lands value even if satellites are never accepted.** Four pieces are already wanted for other reasons, and building them for satellites builds them for those:

- A WAN-identity abstraction — #3676 (multi-WAN with failover) needs the same thing.
- One "authorize a peer by tunnel address and public key" path — #3682 already proposes it; without it, it gets written twice.
- Shared WireGuard key and PSK handling — #3681 already names this as a lift into `shared-libs/`.
- The device fact/policy split — #4007 established the shape; the satellite registry follows it.

**Timing.** This is not a 1.x ask. It reads as 2.0 or later. I am raising it now so the shape can be agreed before any significant code is written, rather than after.

**What I am not asking for.** Not asking anyone to design this, not asking for a 1.x slot, and not asking for hardware changes — the constraint that today's boards cap it at one satellite is accepted as given.

**What I can contribute.** I am willing to do the implementation work. I will also have two physical units and a test environment, which makes me able to exercise a two-router topology end to end — the part of this that is hardest to test without the hardware in hand.

**How I would sequence it**, if that helps make integration cheap: the pieces above that stand alone first, then role and provisioning, then the transport, each as a small reviewable PR, with nothing that changes single-router behaviour.

**Two questions, so this is answerable:**

1. Is this a direction you would accept in principle for a future major release?
2. If so, is the routed-versus-bridged fork (attachment concern 4) a call you want to make before any code is written?

**Prior art.** Consumer mesh (eero, Deco, Orbi) solves coverage with no profile concept. Enterprise (UniFi, Omada, Aruba Instant) solves coverage _and_ per-device policy, but with a controller, RADIUS/802.1X and a supplicant. The gap this sits in is enterprise-grade segmentation with consumer-grade "just type the password" enrollment.

**Related open issues** — all open at time of writing, none merged: #3670 (the auth model this must land on), #3682/#3681 (StartTunnel already has subnets-as-hub, a device registry, a PCP/UPnP gateway, a routed v6 prefix per subnet, and peer auth by tunnel address + public key), #3862 (PD size hardcoded `/48` — the v6 ceiling), #3676 (multi-WAN wants the same WAN-identity abstraction), #3673/#3672 (profile DNS leaks, replicated to a second box), #3667 (neighbor-table trust), #3466 (channel selection across routers), #3662 (backups must carry pairings).

**Attachment.** `SupportingEvidenceForSatelliteRouter.md` — the reasoning behind each alternative, twelve questions that bear on the decision, the implementation concerns surveyed, scope and threat model, prior art, and the file-by-file impact in full.
