# DRAFT ISSUE — for review, not yet opened

**Repo:** `Start9Labs/start-technologies` · **Type:** Feature · **Project:** StartWRT
**Attachment:** `SupportingEvidenceForSatelliteRouter.md`

**Title:** StartWRT: extend Security Profiles across multiple routers (Core + Satellite)

---

A device's Security Profile is decided by its point of entry — the Wi-Fi password it used or the port it plugged into — and every enforcement point for that decision is local to one box: one `hostapd` doing per-PSK dynamic VLAN, one bridge doing VLAN filtering, one firewall zone per profile, one routing table per `vlan_tag`. Profile coverage therefore stops where one router's radio and ports stop.

The workarounds all break the model. An AP or switch behind a LAN port collapses every device behind it into that port's profile (`docs/src/ethernet.md`). A second independent router means two SSIDs, two password sets, two admin UIs, and a second WAN the user does not have. Neither preserves "same password anywhere → same profile."

Goal: one SSID and one set of profiles across two or more routers, no client-side configuration, one place to administer.

## Proposal

One **Core** (owns profiles, holds the only WAN, sole source of truth) and one or more **Satellites** (no WAN, followers). Hub-and-spoke; role fixed at flash.

1. **Entry resolved locally.** A satellite runs the same per-PSK dynamic VLAN and port→VLAN mapping the Core does. No RADIUS, nothing new on the client.
2. **One WireGuard tunnel per profile per satellite**, router-to-router, plus a management tunnel. Count is profiles × satellites, independent of client count.
3. **Routed attachment.** Each satellite owns its own `/24` per profile, Core-allocated, carried in the peer's `allowed_ips`. The tunnel interface joins the profile's `vlan_<iface>` zone — zone membership is keyed on ingress interface (`FirewallZone.network: Vec<String>`, `uciedit/src/openwrt.rs:24`; `profiles.rs:1828 rewrite_firewall`), not on subnet — so existing WAN/LAN access, cross-profile forwarding and VPN chaining apply with no new firewall logic.
4. **Policy and egress at the Core.** Satellites isolate profiles locally and route everything else up.
5. **Semantic config push.** The Core pushes meaning — `{profiles[vlan_tag, policy], passwords[key,vid,label], ports, ssid}` with a monotonic generation — and the satellite regenerates its own subnets/DHCP/zones/routing through the existing `profiles.rs` chain. Not raw UCI, not a backup: the satellite keeps its own identity and never authors profile state.

Pairing is an admin-initiated single-use enrollment code; the satellite's public key is pinned; unpair revokes at the Core.

## Hardware

Both boards available today have one WAN and one LAN port; future hardware is expected to carry four or more LAN ports. First testing: Core WAN → Internet, Core LAN → Satellite **WAN** port, Satellite LAN → a client, everything else over Wi-Fi. Using the satellite's WAN port as the transit uplink keeps its LAN port free for clients, but `ethernet.rs:45` models `wan_port` as the one port that cannot carry a profile, so a satellite needs it repurposed as transit rather than uplink.

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
| `ctrl/src/setup.rs`, `ethernet.rs` | role at flash; transit port on a satellite                                                                                                                                                                                                             |
| `ctrl/src/devices.rs`              | enumerates `ip neigh` (`:207`) and dnsmasq leases (`:240`), both strictly local — needs a satellite-reported read path                                                                                                                                 |
| `ctrl/src/port_control.rs`         | `arrival_matches:980` cannot apply to a routed client; v1 refuses PCP/UPnP explicitly rather than failing silently                                                                                                                                     |
| `web/`                             | Satellites page, pair dialog, staleness indicator; `api.service` trio                                                                                                                                                                                  |

## Work

- [ ] Role + provisioning; daemon gate
- [ ] Site-to-site transport + WAN-less egress — top risk, prove on hardware first
- [ ] Routed attachment at the Core
- [ ] Pairing + remote auth — security review; reconcile with #3670
- [ ] Config sync
- [ ] Capability gating + UI
- [ ] Explicit PCP/UPnP refusal on a satellite
- [ ] Device registry (first satellite→Core direction)
- [ ] IPv6 — prefix delegation over a WG interface is unproven here; spike first
- [ ] Automatic port-forward relay

Design, phased status, test plan and a partial implementation exist and can be contributed. What breaks in each workaround, alternatives with reasons for rejection, and 23 open concerns are in the attached `SupportingEvidenceForSatelliteRouter.md`. Nothing has run on two physical routers; a second unit arrives within two weeks and the first test is one ping from a WAN-less satellite to the Internet.

Related: #3670 (the auth model this must land on), #3682/#3681 (StartTunnel already has subnets-as-hub, a device registry, a PCP/UPnP gateway, a routed v6 prefix per subnet, and peer auth by tunnel address + public key), #3862 (PD size hardcoded `/48` — the v6 ceiling), #3676 (multi-WAN wants the same WAN-identity abstraction), #3673/#3672 (profile DNS leaks, replicated to a second box), #3667 (neighbor-table trust), #3466 (channel selection across routers), #3662 (backups must carry pairings).
