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

**To add as each phase lands:** `vpn_site` AllowedIPs/route generation (prefix, not `/32`); the
routed-attachment source-rule + table-route emission in `profiles.rs`; the semantic-payload
serializer/regenerator; the enrollment-token validation; generation-number monotonicity.

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
10. ⛔ **Staleness / revocation** — remove a password at C1 while S1 is offline; on reconnect S1
    stops accepting it; UI flags the interim staleness.
11. ⛔ **Failover** — Core/tunnel down → S1 clients lose Internet/LAN (expected island); recovery on restore.
12. ⛔ **Unpair** — unpair S1 at C1 → its tunnels drop and its management auth is revoked immediately.
13. ⛔ **Daisy check (negative)** — confirm hub-and-spoke only; a satellite behind a satellite is not supported.

## Security test cases (Layer 3, from design §12)

- ⛔ Rogue satellite: an unpaired device with no valid token cannot enroll or push config.
- ⛔ Rogue Core: a device without the pinned Core key cannot push config to S1.
- ⛔ RPC boundary: S1's config-apply RPC is refused from the LAN/underlay, accepted only over the
  authenticated management tunnel.
- ⛔ Rollback: an older-generation snapshot is rejected.
- ⛔ Tap/inject on the backhaul: only ciphertext observed; injected frames dropped.
