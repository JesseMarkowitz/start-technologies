# Test Plan: StartWRT Satellite Router Support

Companion to `satellite-router.md`. Three layers: (1) host/container **unit** tests, (2) **CLI /
config-generation** functional tests, (3) **on-hardware end-to-end**. Legend: ✅ implemented &
covered · 🟡 implemented, validate on device · ⛔ blocked on unimplemented work (see
`satellite-router-next-steps.md`).

> The full feature can only be proven on two physical BananaPi-F3 routers (a Core + a Satellite).
> Layers 1–2 run on a dev host / in the build container and gate every commit; Layer 3 is the
> acceptance suite once the transport, sync, and auth land.

## Layer 1 — Unit tests (container: `make start-wrt-test`)

Covers the foundation slice landed so far (`backend/ctrl/src/satellite.rs`):

- ✅ `role_defaults_to_core` — an un-provisioned router reads as Core.
- ✅ `parse_role_accepts_known_roles` — `core`/`satellite` parse (case/space-insensitive); junk rejected.
- ✅ `add_then_remove_satellite` — registry add + idempotent remove.
- ✅ `rejects_duplicate_name_and_key` — no duplicate label or public key in the registry.
- ✅ `role_marker_round_trips` — role marker JSON serde round-trip.
- ✅ `sync_snapshot_round_trips` — the semantic snapshot round-trips in camelCase, including the
  country and the per-radio channel plan (D20). Added 2026-09-28.

Site-to-site config generation (`backend/ctrl/src/vpn_site.rs`):

- ✅ `interface_name_is_prefixed` — Core tunnel interface naming.
- ✅ `provisions_interface_peer_zone_and_rule` — parse→provision→assert: the `wg` interface (transit
  `/32`), a subnet-advertising peer, profile-zone membership, and a transit-zone accept rule all
  appear; metadata recorded.
- ✅ `reprovision_is_idempotent` — re-provisioning does not duplicate the peer.

**To add as each phase lands:** the routed-attachment source-rule + table-route emission in
`profiles.rs`; the semantic-payload serializer/regenerator; the enrollment-token validation;
generation-number monotonicity; the Core channel planner (D20: routers that hear each other well
never share a block, pins are honoured, width narrows when blocks run out, the same inputs always give
the same plan); the D18 nftables file parses and its empty set means dark.

## Layer 2 — CLI / config-generation functional tests

Run the `startwrt` binary in `--configs-only` mode against a scratch `--config-root`, or against a
running dev daemon (`STARTWRT_DEV_PASSWORD=…`). Assert on emitted UCI / JSON, no hardware.

- 🟡 2.1 `satellite get-role` on a fresh root → `core`, no endpoint.
- 🟡 2.2 `satellite set-role --role satellite --core-endpoint <addr>` → `/etc/startwrt/role.json`
  written 0600; `get-role` reflects it.
- 🟡 2.3 `satellite pair --name S1 --public-key <b64> --mgmt-address <ip>` on a Core → registry
  entry added; response `status:"registered"` with the not-yet-provisioned note.
- 🟡 2.4 `satellite pair` for a duplicate name or key → `Duplicate` error.
- 🟡 2.5 `satellite list` / `status` → the paired set; counts correct.
- 🟡 2.6 `satellite unpair --name S1` → removed; unpair of a missing name → `NotFound`.
- 🟡 2.7 Core-only gating: on a router whose role is `satellite`, `pair`/`list`/`unpair` → `Authorization` error.
- ⛔ 2.8 (post-transport) `vpn_site` config: creating a satellite tunnel for a profile emits a
  `wg_*` interface with the satellite `/24` in `AllowedIPs`, the tunnel added to the profile's
  `vlan_<iface>` firewall zone, a route for the `/24`, and **no** proxy-ARP for it.
- ⛔ 2.9 (post-transport) accept-rule source zone is the transit-link zone, not `wan`.
- ⛔ 2.10 (post-sync) semantic snapshot → satellite regenerates the same `vlan_tag`/DHCP/zone/routing
  for its own `/24`; generation number increments and is rejected if replayed older.

## Layer 3 — On-hardware end-to-end (Core C1 + Satellite S1, cable backhaul)

1. ⛔ **Provision & role** — flash S1 as Satellite; it boots without running Core-only behaviors
   (no `bootstrap_admin_profile`, no WAN, no `apply_remote_access`).
2. ⛔ **Pair** — enroll S1 at C1 with a single-use token; C1 registry shows S1; the management
   tunnel comes up; unauthenticated enroll attempts are rejected.
3. ⛔ **Tunnels** — per-profile tunnels come up for each profile S1 serves; count = profiles served;
   `wg` shows handshakes.
4. ⛔ **Same password, same profile** — a client using the "Guest" password on **S1** lands in the
   Guest profile: gets an S1-local Guest `/24` address, reaches the Internet **via C1's WAN**, and
   is subject to Guest firewall/DNS/outbound policy — identical to connecting on C1.
5. ⛔ **Ethernet parity** — a device on an S1 port mapped to Guest gets the same treatment.
6. ⛔ **Isolation** — an S1 Guest client cannot reach an Admin-only resource on C1; profiles stay isolated.
7. ⛔ **WAN-less egress (top risk)** — S1 has no WAN; verify the Core-endpoint host route rides the
   transit link, DNS resolves via the Core, and the kill-switch does not strand traffic.
8. ⛔ **MSS/MTU** — a large-payload TCP transfer from an S1 client succeeds (no black-hole);
   confirm the clamp/MTU on the Core ingress.
9. ⛔ **Config propagation** — add/change/delete a profile or password at C1 → S1 converges;
   `generation_applied` advances; the UI shows S1 up-to-date.
10. ⛔ **Staleness / revocation** — remove a password at C1 while S1 is offline. The revoked
    password is never accepted: not while S1 is dark (D18), and not between reconnect and
    reconcile. After reconcile S1 rejects it; UI flags the interim staleness.
11. ⛔ **Failover (D18 fail-closed)** — cut the backhaul: within the liveness threshold S1 stops
    broadcasting the SSID and its profile ports stop serving; its status page stays reachable over
    the uplink. Restore: S1 comes back only after it has reconciled to C1's current generation, and a
    flapping backhaul does not flap the SSID. Separately, pull **C1's WAN** only: S1 must stay up.
    Also: run `fw4 reload` on S1 while it is live and while it is dark. It must come out dark and
    return only when the daemon re-affirms. Record whether going dark restarts the radios: note
    each band's channel before and after a dark/resume cycle.
12. ⛔ **Unpair** — unpair S1 at C1 → its tunnels drop and its management auth is revoked immediately.
13. ⛔ **Daisy check (negative)** — confirm hub-and-spoke only; a satellite behind a satellite is not supported.
14. ⛔ **Country and channels (D20)** — set a country on C1 → S1's `iw reg get` shows it and its
    channel list matches C1's `wifi.regulatory`. Clear it → both fall back to the world subset.
    Pair S1 → it scans while dark, reports, and comes up on the channels C1 planned; `hostapd` on
    S1 never runs automatic selection (its config carries explicit channels). C1's own radios move
    to the plan too. Power-cycle C1 and S1 together five times: the channels are the same every
    time. Pin S1's 5 GHz from C1 → S1 moves and the planner re-plans around it; pin one overlapping
    C1 → C1 warns. Re-plan previews before applying; record how long clients on a moved radio are
    cut off. Check S1's scan sees C1's beacons and their signal.
15. ⛔ **Adoption (D21)** — fresh S1, WAN cabled to a C1 LAN port serving a profile, power on: S1
    brings up no setup Wi-Fi; C1 lists it and asks to convert the port, naming the devices on it;
    nothing changes before approval. Approve, type S1's sticker password → S1 joins. A wrong
    password is refused and does not lock the port.
16. ⛔ **Recovery mode (D22)** — cut the backhaul. On S1's LAN port and, 1 minute later, on
    `StartWRT-Recovery-<suffix>`: `router.lan` shows `Satellite <name> — recovery mode`; nothing else is
    reachable; logs show link and tunnel state but no client leases or associations. Restart
    recovers once the cable is back. Factory reset → S1 reboots broadcasting nothing, the phone
    falls back to the shared SSID on C1, C1 lists S1 as new and offers the saved configuration or a
    new satellite, and S1's sticker password is required again. A phone that knew the shared SSID
    never joins the recovery network by itself; a profile password on the recovery network is
    refused. S1's sticker password is refused on the shared SSID in normal operation.
    With a third router: power C1 off → S1 and S2 both go dark with distinct recovery SSIDs, and each
    accepts only its own sticker password. Reset S1 with its WAN cable pulled → its setup page
    reports no cable and asks satellite-or-main-router; plug the cable in → it takes its setup
    network down and C1 lists it.

## Security test cases (Layer 3, from design §12)

- ⛔ Rogue satellite: an unpaired device with no valid token cannot enroll or push config.
- ⛔ Rogue Core: a device without the pinned Core key cannot push config to S1.
- ⛔ RPC boundary: S1's config-apply RPC is refused from the LAN/underlay, accepted only over the
  authenticated management tunnel.
- ⛔ Rollback: an older-generation snapshot is rejected.
- ⛔ Tap/inject on the backhaul: only ciphertext observed; injected frames dropped.
- ⛔ Non-satellite on the backhaul (D19): a laptop plugged into the backhaul switch, and into a
  cable unplugged from S1, gets no address, no Internet, no LAN and no router UI; the Core flags it.
- ⛔ Switched backhaul (D19): C1 → switch → S1 passes the Layer-3 suite unchanged; with a third
  router, C1 → switch → S1 + S2 does too.
