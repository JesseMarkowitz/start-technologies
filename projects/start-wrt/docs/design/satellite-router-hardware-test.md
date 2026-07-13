# Satellite Router — Basic Hardware Bring-up Test

The **minimal** on-hardware test that validates the core mechanism before the full pairing/sync/UI
is built: a **site-to-site WireGuard tunnel carrying a routed prefix, attached to the Core's profile
zone, gives a WAN-less satellite Internet egress through the Core with that profile's policy.**

This uses the two manual provisioning commands landed on the `start-wrt/satellite-router` branch
(`satellite provision-core-tunnel` / `provision-satellite-tunnel`). Pairing/auth/sync are **not**
needed for this test — the operator generates keys and runs the commands by hand.

> Scope: the commands generate and apply the WireGuard interface + peer + firewall-zone membership +
> routes, and bring the tunnel up. The **underlay** (a static IP on the port that connects the two
> routers, and a firewall zone for it) is basic networking the operator sets up manually — noted
> below. This is intentionally the smallest end-to-end path; it is not the finished feature.

## Hardware

- **Core (C1)** — has the WAN/Internet uplink and a normal, **direct-Internet** profile (e.g.
  `guest`, WAN access = All, outbound = Direct). Pick one **transit port** (e.g. `lan4`).
- **Satellite (S1)** — a second router, **no WAN**. Pick one transit port.
- A cable between the two transit ports.

## Addressing used in this example

| | value |
|---|---|
| Underlay (transit link) | C1 `10.42.0.1/24`, S1 `10.42.0.2/24` |
| Tunnel wg addresses | C1 `192.168.130.1`, S1 `192.168.130.2` |
| Routed to S1 over the tunnel | `192.168.130.2/32` (S1's wg host — for the self-ping test) |
| Core listen port | `51900` |

**Important:** `192.168.130.0/24` must **not** be any Core profile's own subnet (satellites own
their own subnets — the design's routed-attachment). Pick a block the Core doesn't use locally.

## Step 0 — keys (operator, off-router or via `wg`)

```
# On each router (or anywhere):
wg genkey | tee core.key | wg pubkey > core.pub     # CORE_PRIV / CORE_PUB
wg genkey | tee sat.key  | wg pubkey > sat.pub      # SAT_PRIV  / SAT_PUB
wg genpsk > psk                                     # PSK (shared)
```

## Step 1 — underlay + roles (operator, manual basic networking)

On **C1**: give the transit port a static IP and put it in a firewall zone named `transit`
(so the WireGuard accept rule the command adds — `src = transit` — admits the handshake). E.g. via
the UI/UCI: interface `transit` = `10.42.0.1/24` on the transit port; firewall zone `transit`
covering it (input may stay REJECT; the accept rule opens only the WG port).

On **S1**: give its transit port `10.42.0.2/24`, and set the role:

```
startwrt satellite set-role satellite --core-endpoint 10.42.0.1
```

## Step 2 — provision the Core end (on C1)

```
startwrt satellite provision-core-tunnel s1 guest \
  --core-transit-addr 192.168.130.1 \
  --satellite-allowed-ip 192.168.130.2/32 \
  --satellite-public-key "$(cat sat.pub)" \
  --preshared-key "$(cat psk)" \
  --listen-port 51900 \
  --transit-zone transit \
  --core-private-key "$(cat core.key)"
```

This creates `wg` interface `sat_s1_guest` (addr `192.168.130.1/32`, listening on 51900), a peer
advertising `192.168.130.2/32` with `route_allowed_ips=1`, adds `sat_s1_guest` to the `vlan_guest`
zone (so guest policy + WAN egress apply), and opens the handshake on the `transit` zone.

## Step 3 — provision the Satellite end (on S1)

```
startwrt satellite provision-satellite-tunnel s1 guest \
  --sat-wg-addr 192.168.130.2 \
  --core-public-key "$(cat core.pub)" \
  --core-endpoint-host 10.42.0.1 \
  --core-endpoint-port 51900 \
  --preshared-key "$(cat psk)" \
  --sat-private-key "$(cat sat.key)" \
  --allowed-ip 0.0.0.0/0 \
  --firewall-zone-member lan
```

This creates `wg` interface `sat_s1_guest` (addr `192.168.130.2/32`, no listen — it dials), a peer
pointing at `10.42.0.1:51900` with `allowed_ips = 0.0.0.0/0` and `route_allowed_ips=1` (full egress
via the Core), joined to S1's `lan` zone, and brings it up.

## Step 4 — verify (the actual test)

On **S1**:

```
wg show                                   # handshake with C1 present, bytes rx/tx > 0
ping -I 192.168.130.2 1.1.1.1             # Internet via the tunnel -> C1 -> C1's WAN
```

**Pass criteria:** the ping succeeds. That proves, end to end: the site-to-site tunnel carries the
routed prefix; the Core treats the satellite's traffic as `guest` (zone attachment) and NATs it out
its WAN; and a WAN-less satellite reaches the Internet purely through the Core — the core viability
question, on real hardware.

Extra checks:
- On C1, `wg show sat_s1_guest` shows the handshake and the `192.168.130.2/32` allowed-ip.
- Tighten the `guest` profile's WAN access on C1 and confirm S1's egress is filtered the same way
  (proves policy is enforced at the Core, not bypassed).

## Step 5 — a real client behind the satellite (next rung)

The self-ping proves the tunnel + egress. To prove a **downstream device** lands on the profile,
have S1 serve the profile `/24` on one of its ports.

1. **Core:** provision with the whole `/24` routed to the satellite (not just the `/32`):
   re-run Step 2 with `--satellite-allowed-ip 192.168.130.0/24`.
2. **Satellite:** serve the profile `/24` locally on a port (say `lan2`):

   ```
   startwrt satellite provision-satellite-profile psat_guest \
     --vlan-tag 130 --gateway 192.168.130.1 --port lan2 --firewall-zone-member lan
   ```

   This creates interface `psat_guest` (`br-lan.130`, `192.168.130.1/24`), a DHCP pool, puts `lan2`
   on VLAN 130, and joins the `lan` zone.
3. **Verify:** plug a laptop into S1 `lan2`. It should get a `192.168.130.x` lease, reach the
   Internet (through the tunnel → Core → Core WAN), and be subject to the `guest` profile's policy.

> **Addressing caveat (validate on hardware):** the tunnel's own wg address must not overlap the
> served `/24` — for Step 5 give the tunnel a dedicated `/32` (e.g. re-run Step 3 with
> `--sat-wg-addr` outside `192.168.130.0/24`) so `192.168.130.x` routes to the local bridge, not the
> tunnel interface. This is exactly the kind of detail the hardware test exists to shake out.

## What this does NOT yet cover (see `satellite-router-next-steps.md`)

Wi-Fi entry on the satellite (per-PSK), MSS/MTU clamp, automatic pairing/auth, config sync (so
profiles/passwords propagate without manual commands), per-VLAN VPN-routed profiles, roaming, and
the UI. Those are the later phases; this runbook validates the foundation the rest builds on.
