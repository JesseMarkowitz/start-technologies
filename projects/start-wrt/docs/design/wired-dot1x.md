# Design: Wired 802.1X Network Authentication

**Status:** Draft for review
**Scope:** `projects/start-wrt/` only — no OpenWrt submodule changes.

> This is a technical design document. It elaborates the plain-English feature
> brief (`FEATURE_SUMMARY.md`) into an implementable design. Sections marked
> **⟨DECISION NEEDED⟩** hold open questions for the maintainer; they are
> collected at the end.

---

## 1. Overview & Goals

StartWRT assigns each device to a **security profile** (a VLAN + subnet + firewall
zone + DHCP server). Today, wired devices inherit the profile of the physical LAN
port they plug into — anything behind an unmanaged switch on that port shares one
profile. Wi-Fi already does better: each Wi-Fi password maps to a profile via
hostapd per-PSK dynamic VLAN.

This feature brings the Wi-Fi model to Ethernet using **wired 802.1X**
(IEEE 802.1X port-based network access control). A wired device authenticates
with a credential; the credential determines its profile, independent of which
port it uses. Devices that can't or won't authenticate fall back to a
configurable low-privilege default profile.

Two router roles:

- **Core** — holds the on-device RADIUS server, owns the security profiles, and
  acts as the 802.1X **authenticator** on its own LAN ports.
- **Satellite** — a secondary StartWRT router. Its **uplink is a static 802.1Q
  trunk** to the Core (or another Satellite), established administratively at
  pairing — **not** an 802.1X-authenticated link (§3d) — while it acts as an
  **authenticator** on its LAN ports (forwarding client auth to the Core's
  RADIUS). It also rebroadcasts the same Wi-Fi SSID/passwords, so the Core's
  profiles apply network-wide.

Satellites may be arranged **hub-and-spoke** (each connects to the Core) or
**daisy-chained** (Core → Satellite → Satellite …). There is always exactly one
Core, one profile set, one credential set.

### Goals

- Per-credential profile assignment on wired ports, mirroring Wi-Fi.
- Core and Satellite roles, single-PR, entirely within `projects/start-wrt/`.
- Reuse StartWRT's existing TLS cert for the PEAP tunnel.
- Manage everything via GUI, API, and CLI.

### Non-goals

- **Per-device auth behind an unmanaged switch.** 802.1X on a wired port is
  port-level: the first authenticated device sets the port's VLAN; devices behind
  a dumb switch inherit it. Per-device separation requires a Satellite (one
  device per physical port) or a managed 802.1X switch. Documented as a limitation,
  not solved here.
- **MAC Authentication Bypass (MAB) — explicitly rejected, not merely deferred.**
  MAB would assign a *specific non-guest* profile to a device *by MAC address*
  without 802.1X. **We will not pursue it: a MAC address is trivially spoofable, so
  MAB grants profile access on an unauthenticated, forgeable identifier — it
  defeats the security purpose of 802.1X.** Devices that can't speak 802.1X are
  handled the safe way instead: they land in the **guest/default profile** (the
  "never stranded" fallback), which is low-privilege by design. Note the guest
  fallback is a *different* mechanism from MAB — the **guest VLAN is required** (open
  item §12-B) and does not rely on MAC identity at all.
- **Web UI for RADIUS internals** — a raw editor for the RADIUS config, user DB,
  clients file, ports, or certificates. This is a non-goal **because those
  internals are derived, not authored**: the user DB is generated from profile
  passwords (§4b), the clients file and shared secrets from pairing (§5a), and the
  certs are the reused StartWRT certs (§9). Hand-editing any of them would be
  silently overwritten on the next regeneration, so exposing them as editable would
  be a footgun. What an admin *does* legitimately need — **is a device
  authenticating, and if not, why** — is served by `dot1x.status` (live per-port
  client list), a recent-auth-events view (§6), and a **`dot1x.logs` action that
  streams the `hostapd-radius` log through the API/UI so SSH is not required** (§4a).
  Shared-secret rotation is a pairing action, not a text field. See §9.

---

## 2. Terminology

| Term | Meaning |
|---|---|
| **Core** | Router running the RADIUS server + authenticator on its LAN ports. One per network. |
| **Satellite** | Secondary router: static trunk uplink (no WAN 802.1X), authenticator on LAN. |
| **Authenticator** | The 802.1X role that guards a port and relays EAP to RADIUS (hostapd, `driver=wired`). |
| **Supplicant** | The 802.1X role that proves identity to an upstream authenticator (wpa_supplicant, `-D wired`). *StartWRT does not run one — the inter-router uplink is a static trunk, not an 802.1X link (§3d, §12-C); listed for reference.* |
| **Security profile** | Existing StartWRT construct: VLAN tag + subnet + zone + DHCP. See `profiles.rs`. |
| **Network credential** | The profile's existing Wi-Fi password (label = username, key = password), reused for wired 802.1X — not a new field (§4b). |
| **Default / guest profile** | The profile a port falls back to when a device doesn't authenticate. |

---

## 3. Architecture

### 3a. Wired client authenticating on a Core LAN port

```
[client]---eth0(br-lan)---[Core hostapd driver=wired]
   |  EAPOL-Start                    |
   |------------------------------->  | EAP-PEAP/MSCHAPv2
   |                                  |---RADIUS Access-Request--->[hostapd-radius 127.0.0.1:1812]
   |                                  |<--Access-Accept + Tunnel-Private-Group-ID=<vlan>--|
   |   port AUTHORIZED, hostapd       |
   |   assigns iface to VLAN <n> via  |
   |   FULL_DYNAMIC_VLAN + vlan_bridge |
   |<---- DHCP from profile <n> ------|
```

hostapd (full variant) with `CONFIG_FULL_DYNAMIC_VLAN` creates an 802.1Q
sub-interface for the RADIUS-returned VLAN and attaches it to `br-lan`
(`vlan_bridge`). No `bridge vlan add`; standard kernel netlink. This is the same
mechanism Wi-Fi dynamic VLAN already uses on this hardware.

### 3b. Satellite uplink — a static trunk, no upstream 802.1X

The Satellite's uplink to the Core (or to an upstream Satellite) is the **trusted
802.1Q trunk of §3d**, authorized administratively at **pairing** and persisted in
UCI — it is **not** an 802.1X-authenticated link. Consequences:

- The Satellite runs **no WAN supplicant**. Nothing on the uplink has to pass an
  EAP challenge before traffic flows; the trunk's L2 (management VLAN + all profile
  VLANs) is up as soon as the cable links, independent of the Core being reachable.
- There is therefore **no DHCP-timing race** on the uplink. The Satellite reaches
  the Core over the flat management VLAN (§3c) as provisioned at pairing and
  addresses the Core's RADIUS directly.
- The Satellite runs its **own authenticator** on its LAN ports and relays each
  client's EAP to the Core RADIUS (§3c); an unauthenticated client lands in that
  LAN port's guest VLAN, tagged as guest over the trunk — it never inherits the
  Satellite's or any other profile.

This is the deliberate outcome of the trust decision in §3d / §12-C: per-frame
802.1X on the inter-router link would buy resistance only to a live device-swap on
that specific cable, at the cost of an entire supplicant + DHCP-sequencing
subsystem, so we rely on physical security of the trunk cable instead (MACsec is
the future cryptographic upgrade — §9, §12-J).

### 3c. Wired client on a Satellite LAN port → Core RADIUS

```
[client]--[Satellite hostapd driver=wired]--...--[Core network]--[Core hostapd-radius]
                    |  auth_server_addr = <Core LAN IP>:1812
                    |  auth_server_shared_secret = <pairing secret>
   Access-Request travels up the chain (hub-and-spoke: 1 hop;
   daisy-chain: each hop's own profile VLANs carry RADIUS toward the Core).
```

The Satellite's authenticator points its RADIUS client at the Core's RADIUS
server over the network instead of at localhost. VLAN assignment returned by the
Core is applied locally on the Satellite's bridge, so the client lands in the
same profile it would on the Core.

**Daisy-chain / how RADIUS reaches the Core across N hops (RESOLVED).** The
management VLAN is a **single flat layer-2 segment that spans the whole chain**:
every trunk carries the management VLAN tagged, and each intermediate Satellite
simply **bridges** it (no routing, no RADIUS proxying). So all StartWRT routers —
Core and every Satellite, however deep — sit on **one management subnet** and can
address the Core's RADIUS directly. A Satellite's authenticator sends its
Access-Request to the Core's fixed management IP; the frame is L2-bridged hop by
hop up the chain and back. Each Satellite is authorized once in the Core's
`radius.clients` by its own management-VLAN IP + secret.

Why flat-L2 rather than routed-per-hop: it needs zero per-hop RADIUS
configuration, no proxy state, and no re-authorization at each hop — every router
already trusts the management VLAN by construction (§3d). The tradeoff is that the
management VLAN is one broadcast domain across the chain; that is bounded and fine
at StartWRT's scale (a handful of routers), and it keeps the deepest Satellite's
auth path identical to a one-hop Satellite's. The only per-hop requirement is that
each intermediate Satellite tags the management VLAN onto its downstream trunk —
which pairing already does (§5a).

### 3d. Trust boundary at the Satellite uplink — the privilege-inheritance problem

**This is the central security decision for the Satellite role.** Consider a
Satellite plugged into a Core LAN port:

1. The Core port runs 802.1X. The Satellite authenticates upstream and the port
   is authorized.
2. From then on, the Core treats that port as belonging to whatever profile the
   Satellite authenticated into.

If the Satellite simply *bridges* its downstream clients onto that one authorized
uplink, then **every device behind the Satellite — including a device that never
authenticated — inherits the Satellite's profile.** If the Satellite's own
credential maps to a high-privilege profile, an unauthenticated laptop plugged
into the Satellite silently gets high privilege. That is a privilege-escalation
hole, and it is exactly the failure the maintainer flagged.

#### The approach: a trusted VLAN trunk

The Core↔Satellite link is an **802.1Q trunk** carrying *all* profile VLANs
(tagged), authorized **administratively during pairing** rather than by per-frame
802.1X. The Satellite runs its **own authenticator** on its LAN ports, relays
each client's EAP to the Core's RADIUS, and tags that client's traffic into the
client's *own* VLAN before it crosses the trunk. Consequences:

- An authenticated device on the Satellite lands in its real profile (goal met).
- An **unauthenticated** device on the Satellite gets the Satellite port's
  **guest/default VLAN**, tagged as guest over the trunk — it does **not** inherit
  the Satellite's privilege. Hole closed.
- The Satellite's "own" credential/identity is used only for the Satellite's
  management traffic and to establish trunk trust — never as the profile for
  forwarded client traffic.

The cost is that the Core must treat a Satellite uplink port differently from a
client port (trunk vs single dynamic VLAN), and the trunk's trust must be
bootstrapped by **pairing** (a Satellite is an *infrastructure* peer, not an
ordinary client). This is why the role model, the per-port mode, and the pairing
flow are one coupled decision, not three independent ones:

| Core LAN port mode | Behavior | Trust source |
|---|---|---|
| **Static** | fixed profile (today's default) | admin config |
| **802.1X client** | single dynamic VLAN from the device's own auth; guest fallback | RADIUS (per device) |
| **Satellite uplink (trunk)** | trusted 802.1Q trunk carrying all profile VLANs | pairing (per Satellite) |

> **Implication for the brief:** the "Satellite = supplicant on WAN" framing is
> **superseded** (DECIDED — §12-C). The uplink runs **no per-frame 802.1X and the
> Satellite runs no WAN supplicant**; trunk trust is purely administrative
> (established once at pairing). The per-client authentication that determines each
> device's profile happens on the Satellite's own ports (relayed to the Core
> RADIUS), and the uplink is a trunk — not a single authorized access VLAN.

#### Mechanics (what must be built)

- **Port designation.** The Core LAN port a Satellite connects to is set to
  `SatelliteUplink` during pairing. On that port the Core runs **no per-client
  802.1X**; it configures the port as a **tagged (802.1Q) trunk** member of
  `br-lan` carrying every profile VLAN plus the management VLAN. This is a
  `NetworkBridgeVlan`-style write (`ethernet.rs`) that adds the port as a tagged
  member of all VLANs, rather than the single-VLAN dynamic assignment used for
  client ports.

- **Trunk trust — what "trusted" means and its risk.** On a `SatelliteUplink`
  port the Core does **not** re-authenticate each frame; it accepts the 802.1Q VLAN
  tag on every incoming frame as authoritative (that is what makes it a trunk). The
  Satellite is responsible for tagging honestly — it only tags a client's frames
  into a VLAN after that client authenticated. Trust is established once, at pairing
  (admin paired *this* Satellite and designated *this* port), and thereafter the trunk is trusted
  by configuration — there is no per-frame 802.1X and no supplicant on the link.

  The residual risk is **physical**: because the port trusts tags, anyone with
  physical access to *that specific cable* — splicing it, tapping it, or unplugging
  the Satellite and plugging in their own device — could inject frames tagged for
  **any** VLAN and thereby reach **any** profile, including high-privilege ones
  (classic "VLAN hopping"). Note this raises the stakes versus a normal LAN cable:
  an ordinary StartWRT LAN cable already grants *its port's one profile* to whoever
  plugs in, but a trunk cable carries *all* profiles, so a physical attacker on the
  trunk reaches everything rather than one lane.

  **Link-layer encryption (MACsec, 802.1AE) is what would close this**, and it is
  **out of scope** for this feature. MACsec cryptographically authenticates and
  encrypts every frame on the link using a key only the paired Satellite holds, so
  tapped or injected frames are rejected — the trunk would then be trusted
  *because* of the key, not because of the cable. We omit it for now and instead
  rely on **physical security of the inter-router cable**, which is the same trust
  posture StartWRT already assumes for LAN cabling. MACsec is a clean future
  enhancement (the SpaceMiT K1 MAC and mainline kernel support it) and is noted as
  such in §9. The practical guidance for users: run inter-router trunk cables inside
  the physically-controlled premises, exactly as you would trust an in-wall LAN run.

- **Management VLAN.** A dedicated VLAN (a reserved profile every StartWRT trusts)
  carries RADIUS (1812/1813) and management RPC between routers over the trunk. See
  §3c for how it spans a daisy-chain. Firewall: RADIUS/management is reachable only
  within this VLAN, never from a client profile or WAN (§9).

- **Client path on the Satellite.** The Satellite's LAN ports are ordinary
  `Dot1xClient` ports (its own authenticator), `auth_server_addr` = the Core over
  the management VLAN. Each authenticated client is tagged into its VLAN; the tag
  survives across the trunk unchanged, so the client lands in the identical
  profile it would on the Core. Unauthenticated → that port's guest VLAN.

- **Pairing** (§5a) is the mechanism that provisions all of the above: shared
  secret, Core CA/trust, management VLAN ID, the Satellite's identity,
  `radius.clients` entry, and the Core port flip to trunk.

---

## 4. Backend Design (Rust, `backend/ctrl/`)

### 4a. New module `dot1x.rs`

Follows the handler convention (`backend/AGENTS.md`): export
`pub fn dot1x<C: CtrlContext + Clone>() -> ParentHandler<C>` (the `+ Clone` bound
matches every existing handler, e.g. `ethernet<C: CtrlContext + Clone>`,
`ethernet.rs:93`), registered as `.subcommand("dot1x", dot1x::dot1x::<C>())` in
`main_api()` (`lib.rs:397-425`).

```rust
// dot1x.rs
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Dot1xRole { Core, Satellite }   // DECIDED: Model B (§3d)

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PortAuthMode {
    Static,                              // fixed profile (today's default)
    Dot1xClient { guest: ProfileId },    // per-device auth; guest REQUIRED (explicit, §4c)
    SatelliteUplink,                     // trusted 802.1Q trunk to a paired Satellite (Model B, §3d)
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Dot1xConfig {
    pub enabled: bool,
    pub role: Dot1xRole,
    /// Satellite-only, read-only: identity + Core reachability provisioned by
    /// pairing (§5a), not user-entered. Absent on a Core or an unpaired device.
    pub upstream: Option<UpstreamInfo>,   // { identity, core_mgmt_addr, mgmt_vlan }
    /// Per LAN port: authentication mode (guest profile is carried inside
    /// PortAuthMode::Dot1xClient — see §4c).
    pub ports: BTreeMap<String, PortAuthMode>,
}
```

Endpoints (all `from_fn_async_local`, `.with_display_serializable()`):

| Endpoint | Purpose |
|---|---|
| `dot1x.get` | Read `Dot1xConfig` from UCI (`startwrt`/`radius`/`network`). |
| `dot1x.set` | Write UCI, regenerate RADIUS users, write hostapd-wired authenticator configs, reload services (only when `ctx.effectful()`). |
| `dot1x.status` | Runtime state via `ubus call hostapd.<iface> get_clients` / `get_status` (wired ubus object, validated present). |
| `dot1x.logs` | Return recent `hostapd-radius` log lines so an admin can diagnose auth failures **without SSH**. Backed by `logread` filtered to the RADIUS/hostapd source; optional `lines`/`follow` params. Mirrors how other StartWRT log surfaces are read. |

### 4b. Credential source — reuse the Wi-Fi password (DECIDED)

**Decision:** the wired 802.1X credential *is* the profile's existing Wi-Fi
password. No new credential field on the profile. This unifies the story — a
device uses the *same* secret for wired 802.1X and for Wi-Fi, and it lands in the
same profile either way.

Concretely, StartWRT already models profile passwords as `WifiStation` sections
(`wireless` config: `key` → `vid` → profile), surfaced as the `Password { label,
profile, password }` set in `wifi.rs`. The 802.1X user database is derived from
that same set:

- **username** = the password's `label` (defaults to the profile `fullname`;
  must be unique per RADIUS user — see open questions on multi-password profiles)
- **password** = the `WifiStation.key`
- **vlan-id** = the profile's `vlan_tag` (via `WifiStation.vid` / `Lookup`)

Admin passwords (`profile = None`) are **excluded** — they are not network-access
credentials.

**`regenerate_radius_users(cfgs)`** — new routine that iterates the same
`WifiStation`/`Password` set the Wi-Fi path uses (`wifi.rs:403-432`) and emits a
`phase2.users` entry per profile-bound password into `/etc/radius/users`:

```json
{
  "phase1": { "wildcard": [ { "name": "*", "methods": ["PEAP"] } ] },
  "phase2": { "users": {
    "<dot1x_username>": { "password": "<dot1x_password>", "methods": ["MSCHAPV2"], "vlan-id": <profile.vlan_tag> }
  } }
}
```

> **Validated against `radius.c`:** user-attr keys are exactly `password`,
> `hash`, `salt`, `methods`, `radius`, `vlan-id` (int, <4096), `max-rate-up/down`.
> `vlan-id` is emitted as RADIUS Tunnel-Private-Group-ID. **Correction vs. earlier
> plan:** with a PEAP outer method, the phase2 inner method is **`MSCHAPV2`**
> (EAP-MSCHAPv2), not `TTLS-MSCHAPV2`.

Call sites that must trigger regeneration: `profiles.create`, `profiles.set`
(when credential changes), `profiles.delete`, and `dot1x.set`. Use `uciedit`'s
existing retry mechanism (profile writes already retry) rather than an ad-hoc loop.

The existing `profiles::Lookup` (`profiles.rs:2693`, `from_vlan(vid)`) maps VLAN→
profile and is reused wherever we translate between credential, VLAN, and profile.

### 4c. `ethernet.rs` — port authentication mode

Extend the `Port` struct (`ethernet.rs:37`) with the auth mode; the guest profile
lives *inside* `Dot1xClient` (not the existing `profile` field) because it is
**required** for that mode:

```rust
pub struct Port<Id: Ord = ProfileId> {
    pub profile: Option<Id>,                 // existing: Static-mode assignment
    #[serde(default)] pub auth_mode: PortAuthMode<Id>,  // Static | Dot1xClient{guest} | SatelliteUplink
}
```

Note `Port` is **generic over `Id`** (`ethernet.rs:37`), so `PortAuthMode` and its
embedded guest profile must be **`PortAuthMode<Id>`** / `Dot1xClient { guest: Id }`
— using the same `Id` parameter as `profile` so the guest reference round-trips
through the same id mapping the existing `profile` field does (the `dot1x.rs`
listing above shows the default `ProfileId` instantiation). Defaulting `auth_mode`
via `#[serde(default)] → Static` keeps existing `Port` payloads deserializing
unchanged.

**Guest profile is an explicit, required choice (DECIDED).** When a port is set to
`Dot1xClient`, the admin must pick a guest profile — `ethernet.set` rejects the
write if `Dot1xClient { guest }` is missing (no silent inheritance of the prior
static profile). A successfully authenticated device gets its credential-derived
VLAN; an unauthenticated/failed device gets `guest`. `ethernet.set` writes the
hostapd-wired config section for the port and reloads hostapd. `Static` ports keep
the current bridge-vlan path unchanged (`NetworkBridgeVlan`, `ethernet.rs:120`);
`SatelliteUplink` ports are written as tagged trunk members (§3d mechanics).

### 4d. `uciedit` — typed `radius` section

Add to `backend/uciedit/src/openwrt.rs` (matches the shipped `radius.config`
schema + StartWRT cert paths):

```rust
#[derive(Debug, TypedSection, Default)]
#[uci(ty = "radius")]
pub struct RadiusConfig {
    #[uci(default_value = true)] pub disabled: bool,
    #[uci(default)] pub ipv6: Option<String>,
    #[uci(default)] pub log_level: Option<String>,
    pub ca_cert: String,
    pub cert: String,
    pub key: String,
    pub users: String,
    pub clients: String,
    #[uci(default)] pub auth_port: Option<u16>,
    #[uci(default)] pub acct_port: Option<u16>,
    #[uci(default)] pub identity: Option<String>,
}
```

### 4e. Effectfulness / reload rules

Per `backend/AGENTS.md`: after UCI writes, reload the relevant init script
(`/etc/init.d/radius reload`, `/etc/init.d/network reload`, hostapd reload) only
when `ctx.effectful()` is true, so `--configs-only` CLI mode stays pure. Use
`run_quiet_async` (`lib.rs:437`) for service reloads / ifup-like calls that may
spawn long-lived children.

---

## 5. On-Device Runtime

### Files (staged vs. generated)

| Path | Origin | Notes |
|---|---|---|
| `/etc/config/radius` | staged from `backend/firstboot_config/radius` (new) | disabled by default; points at StartWRT certs |
| `/etc/radius/users` | generated at runtime by `regenerate_radius_users` | JSON user DB |
| `/etc/radius/clients` | generated / staged | `<CIDR> <secret>` lines (validated format) |
| `/etc/hostapd-wired-<iface>.conf` | generated by `dot1x.set`/`ethernet.set` | authenticator config per LAN port |

### Persistence: backups & upgrades

`backup.rs` produces its archive with `sysupgrade --create-backup` (`backup.rs:55`),
and a preserve-config `sysupgrade` keeps the same set — so the rule is the same for
both **backup** and **firmware upgrade**:

- **UCI config is captured and preserved automatically.** OpenWrt's default keep
  set includes `/etc/config/*`, so `/etc/config/radius`, the new `dot1x` sections in
  `/etc/config/startwrt`, and the **`config satellite` registry** ride backups and
  survive upgrades with **no keep.d entry needed**. This is why pairing state
  (per-Satellite secrets, topology, slot identities) lives in UCI: it is the one
  category that is **not derivable** and must persist — keeping it in UCI gets that
  for free.
- **Generated non-UCI files are neither backed up nor preserved — and that's
  correct.** `/etc/radius/users`, `/etc/radius/clients`, and
  `/etc/hostapd-wired-*.conf` are **100% derived** (users from profile
  passwords §4b; clients from the `satellite` registry; hostapd from the
  dot1x UCI). They are regenerated at boot / on `dot1x.set`, so they behave like any
  other generated runtime artifact. **Do not add them to keep.d** — a stale kept copy
  would only mask a regeneration bug. The single hazard to avoid: never let a
  generated `/etc/radius/*` file be the *only* copy of a secret — every secret's
  source of truth is UCI.
- **Boot regeneration is required.** Because a preserve-config upgrade keeps UCI but
  wipes `/etc/radius/*`, the Core's init path must **regenerate `users`/`clients`
  from UCI on startup** (not assume the files survived). Cheap: it's the same
  routine `dot1x.set` calls.
- **Reflash / factory reset** (no config-keep) wipes UCI → pairing is lost →
  re-pair, or restore a backup. This matches normal router behavior and needs no
  special handling.
- **New concerns to bake in:** (1) `stage-files.sh` must `mkdir /etc/radius/` so
  runtime writes have a home; (2) `users`/`clients` hold plaintext secrets → write
  **mode 0600 root**; (3) regenerate **atomically** (temp + rename, as
  `device_names.json` already does) so a live backup or a reload never reads a
  half-written file.

### RADIUS server config (Core)

`/etc/config/radius` points at existing certs (validated `ssl.rs` helpers):

```
option ca_cert '/etc/ssl/certs/startwrt-ca.pem'      # ca_cert_path()
option cert    '/etc/ssl/certs/startwrt-server.pem'  # server_cert_path()
option key     '/etc/ssl/private/startwrt-server.key'# server_key_path()
option users   '/etc/radius/users'
option clients '/etc/radius/clients'
```

`radius.init` invokes `hostapd-radius -C ca -c cert -k key -s clients -u users
-p 1812 -P 1813 -i <identity>` (validated). PEAP presents `cert` to clients;
clients that already trust the StartWRT CA validate it with no extra setup.

### Authenticator config (hostapd, `driver=wired`)

Per dot1x LAN port: `driver=wired`, `interface=<port>`, `ieee8021x=1`,
`auth_server_addr` = `127.0.0.1` (Core) or Core LAN IP (Satellite),
`auth_server_shared_secret` = pairing secret, `dynamic_vlan=1`,
`vlan_bridge=br-lan`, `vlan_tagged_interface=<port>`.

### Uplink bring-up (Satellite) — no supplicant, no DHCP race

The uplink is a **static administrative trunk** (§3b / §3d), not an
802.1X-authenticated link, so there is **no wired supplicant and no
"DHCP-before-authorization" race** to solve. `firstboot_config/network` brings the
uplink up as a **tagged trunk member** (management VLAN + profile VLANs) rather
than `proto dhcp` on a raw port; the Satellite reaches the Core over the flat
management VLAN as provisioned at pairing (§5a) — no `wpa_supplicant`, no
`dot1x-action.sh`, no hold-down timer.

> Per-frame 802.1X on the uplink *would* have required a WAN supplicant plus the
> deferred-DHCP action-script machinery (an `-a /etc/dot1x-action.sh` hook driving
> `ifup wan` on `CONNECTED`, with a best-effort guest-on-failure path and a
> no-event hold-down timer). That whole subsystem was evaluated and **rejected** —
> see §12-C for the rationale.

### Service lifecycle / ordering & power-failure convergence

**Design for convergence, not strict boot order** — we cannot control the order in
which routers regain power, so nothing may *depend* on a specific sequence.

- **Core.** `radius` (START=30) and the LAN authenticators start as ordinary init
  services with **no dependency on any Satellite**. Because a `SatelliteUplink`
  trunk is authorized **administratively at pairing and persisted in UCI** (§3d), the
  Core's trunk ports come up **immediately at boot** — they do *not* wait for the
  Satellite to authenticate. hostapd reloads on port-config change.
- **Satellite boots before the Core.** Its uplink trunk is likewise static config,
  so L2 (management + profile VLANs) is up as soon as the links are. Its own LAN
  authenticator starts, but Access-Requests to the Core RADIUS fail until the Core
  is reachable → those clients land in **guest** meanwhile.

  **Convergence latency — do not rely on passive timers.** Left at defaults the
  re-auth gap is unacceptably long: hostapd `eap_reauth_period` defaults to **3600 s
  (1 hour)**, and once hostapd has marked the Core RADIUS dead it re-probes it only
  on a multi-minute interval; the fastest passive path is the *client's own*
  EAPOL-Start retry (~60 s after a failure), which we don't control. So the design
  must **actively nudge**: the Satellite already tracks `core_reachable` (§5a
  monitoring); on a **false→true** transition it immediately **de-authenticates the
  guest-parked clients / reloads its authenticator**, forcing every affected device
  to re-run EAPOL **within seconds**. Secondary safety net: lower hostapd's
  dead-RADIUS retry interval to ~60 s so anything the nudge misses still recovers
  quickly. Result: event-driven convergence in **seconds**, not a timer-driven gap
  of minutes-to-an-hour.
- **Daisy-chain cascade.** B's trunk depends on A's trunk which depends on the Core
  — but since **every trunk is static**, the chain's L2 comes up **bottom-up as each
  link powers on, independent of the Core being up**. Only *end-client RADIUS auth*
  waits for the Core; it converges once the Core is reachable. No strict order
  required at any depth.

> **Reconciliation — DECIDED: administrative trunk, no uplink 802.1X (§12-C).**
> The Satellite uplink is a static administrative trunk (§3d, "trust established at
> pairing"), so the Satellite needs **no WAN supplicant** and none of the
> DHCP-timing / action-script machinery applies to the uplink — the trunk's L2 is
> up as soon as the cable links, regardless of Core reachability or power order.
> (A StartWRT router acting as a plain 802.1X client on a *foreign* network is a
> separate, out-of-scope case.) This is what makes the convergence story above hold
> at every depth: only *end-client RADIUS auth* waits for the Core, never the
> inter-router links themselves.

### 5a. Satellite pairing / enrollment — FULL SPEC

Automated pairing establishes the trunk trust (§3d) and provisions every secret.
The **Core is always the single authority**: every Satellite — however many, and
however deep in a daisy-chain — is registered centrally at the Core and terminates
its RADIUS at the Core. The only thing that varies is *which router owns the
physical port* the new Satellite cables into: the **Core** (hub-and-spoke) or an
**already-paired Satellite** (daisy-chain). That router is the **upstream router**
for this enrollment; it performs the bootstrap-port dance under Core direction.

#### Trust bootstrap (the one hard part)

Before pairing, the new Satellite has no way to trust the Core's TLS cert. The
one-time token carries the Core CA **fingerprint**, so the Satellite validates the
Core cert against it and trusts the Core *before* receiving the full CA. The
`bootstrap_code` in the token authenticates the Satellite to the Core in return.
This is a mutual, single-use bootstrap; the token expires.

#### Flow (generalized over the upstream router U)

1. **Core: "Add Satellite."** Admin picks the **upstream router U** (the Core
   itself, or an existing Satellite) and **which LAN port on U** the new Satellite
   will use. The Core ensures a per-network base exists (RADIUS running; management
   VLAN reserved) and mints a **fresh per-Satellite shared secret**
   (`generate_password`, `lib.rs`). It issues a one-time token
   `{ core_mgmt_address, ca_fingerprint, bootstrap_code, mgmt_vlan, expiry }` and
   instructs U to put U's chosen port into a temporary **management-VLAN-only
   access** state (not yet a trunk) so the freshly-cabled Satellite can reach the
   Core across U (and, in a chain, across every trunk between U and the Core).
2. **Satellite: "Join as Satellite."** Admin pastes the token (or scans the QR)
   and confirms its uplink port. The Satellite dials `core_mgmt_address` over TLS,
   validates against `ca_fingerprint`, presents `bootstrap_code`.
3. **Core verifies** and returns over the trusted channel: the per-Satellite RADIUS
   shared secret, the full Core CA, the management VLAN id, the Satellite's assigned
   identity/mgmt-IP, and the profile→VLAN map for the trunk.
4. **Core commits:** appends `<this-Satellite mgmt-IP/32> <its-secret>` to
   `/etc/radius/clients`, records a `config satellite` registry entry, and
   instructs **U** to **flip U's port to `SatelliteUplink`** (tagged trunk over all
   profile VLANs + management VLAN).
5. **Satellite commits:** installs the Core CA, sets its authenticator
   `auth_server_addr` = Core-over-management-VLAN with its secret, configures its
   uplink as a trunk member, brings up the management VLAN, starts its own
   authenticator. It is now itself eligible to be an **upstream router** for a
   further downstream Satellite (daisy-chain).
6. **Verify:** the Satellite reaches the Core RADIUS across the chain; a test client
   authenticates end-to-end into the correct profile.

#### Multiple Satellites & daisy-chains — how N is handled

- **Central registry on the Core.** One `config satellite` UCI section per paired
  Satellite: `identity`, `mgmt_ip`, `upstream_router` (Core or a Satellite id),
  `upstream_port`, `shared_secret`, `status`, `added_at`. `radius.clients` gets
  **one line per Satellite** with its **own** secret — compromise of one Satellite
  never exposes another. `dot1x.satellites` lists them with live status.
- **Hub-and-spoke** = every Satellite's `upstream_router` is the Core, each on its
  own Core LAN port (each a trunk). Bounded by the Core's LAN port count; beyond
  that, chain.
- **Daisy-chain** = a Satellite's `upstream_router` is another Satellite. The
  intermediate Satellite forwards the management VLAN + all profile VLANs across
  its trunk, so the deepest Satellite's clients still authenticate at the Core.
  Enrollment of a downstream Satellite is driven from the Core exactly as above,
  with U = the intermediate Satellite; the Core instructs U (over the management
  VLAN) to run the temporary-access→trunk port dance. **Trust is not delegated** —
  U never mints secrets or terminates RADIUS; it only relays and toggles its own
  downstream port on Core instruction.
- **Removal / revocation.** `dot1x.satellite-remove` drops the `radius.clients`
  line + registry entry and instructs U to return the port to Static. A chain is
  removed from the leaf inward (removing an intermediate orphans its descendants —
  the API rejects removing a Satellite that still has children, or cascades with
  confirmation).

#### Health monitoring & fault localization (design this in now)

A daisy-chain has a structural blind spot: if intermediate Satellite **A** fails,
its descendant **B** is cut off from the Core — but so is A's own reporting path,
so **from the Core's view A and B go dark simultaneously** and the Core cannot, by
itself, tell whether A failed, B failed, or the A–B link dropped. We can't remove
that blind spot (there's no other path to B), but three cheap-now / painful-later
mechanisms make it diagnosable:

- **Per-Satellite heartbeat + `last_seen`** in the `config satellite` registry.
  Each Satellite periodically checks in to the Core over the management VLAN; the
  Core stamps `last_seen`. A gap flags an outage.
- **Reachability booleans in `dot1x.status`.** Each router reports `upstream_up`
  (can reach its parent) and `core_reachable` (can reach the Core RADIUS/mgmt IP).
  A router that still has its parent but lost the Core localizes the break to
  *above* the parent.
- **Topology in the registry** (`upstream_router` parent pointers, already
  present). The UI renders the chain, so "A and everything below A unreachable"
  visually points at A or the Core–A link. Pair this with a **small persistent
  local event ring-buffer** (link up/down + auth events, retrievable via
  `dot1x.logs`) so a router that was isolated can be interrogated for its history
  once reconnected or reached directly.

#### Replacing a router in a chain (design this in now)

Replacing intermediate **A** in Core→A→B must **not** force re-pairing B. Two
design choices make replacement a single operation instead of a teardown:

- **Logical slot identity, not hardware identity.** A registry entry is a *slot*
  (`identity`, `mgmt_ip`, parent, children, secret) that is **not bound to the
  device MAC/serial**. Replacing A = re-keying A's existing slot onto new hardware:
  mint a slot-scoped token, enroll the new device into A's slot, revoke the old
  secret. B's entry (parent = A's slot) is untouched.
- **B already targets the Core, not A.** Because the management VLAN is one flat L2
  segment and B's authenticator addresses the **Core's** RADIUS directly (§3c), B
  never depended on A's *identity* — only on A *bridging* the management + profile
  VLANs. The replacement A only has to re-establish that trunk (which pairing does),
  and B keeps working with no reconfig. A `dot1x.satellite-replace` endpoint (or
  `remove-into-slot` + `enroll`) formalizes this while preserving children.

#### Pairing RPC surface (Core unless noted)

| Endpoint | Role | Purpose |
|---|---|---|
| `dot1x.satellite-token` | Core | mint one-time token for a chosen `(upstream_router, port)`; sets U's port to bootstrap-access |
| `dot1x.satellite-enroll` | Core | called *by the joining Satellite*; verifies `bootstrap_code`; returns the provisioning bundle; writes `radius.clients` + registry; tells U to trunk the port |
| `dot1x.satellite-join` | Satellite | admin-facing; consumes a token, dials the Core, applies the bundle |
| `dot1x.satellites` | Core | list paired Satellites + status |
| `dot1x.satellite-remove` | Core | revoke a Satellite (leaf-first / cascade) |
| `dot1x.satellite-replace` | Core | re-key an existing slot onto new hardware, preserving children |

#### Resolved sub-decisions

- **Token delivery format — DECIDED: both text and QR.** The mint step returns the
  token as a copy-paste text code **and** a QR encoding the identical payload. Text
  serves two-browser-tab copy/paste (Core tab → Satellite tab); QR serves phone
  scan (photograph the Core's screen, or the joining Satellite displays a join
  screen). Same bytes either way.
- **Bootstrap-access window timeout — DECIDED: default 15 min, UI-configurable.**
  This is the interval the unpaired Satellite's temporary join endpoint (and U's
  temporary management-VLAN-only port state) stays open before **auto-revert** back
  to the locked/Static default, so a half-finished pairing never leaves an open
  bootstrap door. 15 min is enough to walk to the other router and paste/scan, short
  enough to bound exposure; the admin can override per pairing session (suggested
  range 5–60 min). Distinct from the **token expiry** in the token payload, which is
  the Core-side validity of the issued credential — both must be live for pairing to
  complete.
- **Management-VLAN numbering — RESOLVED in §5b.**

### 5b. Management-VLAN numbering (RESOLVED)

**Existing StartWRT allocation scheme** (`profiles.rs:1541-1551`): a new profile's
VLAN tag is either admin-chosen (rejected if it duplicates an existing tag) or
**auto-allocated from the range `101..4095`, skipping any tag already in the network
config**. VLAN `1` is the bridge primary/default (`ensure_vlan_filtering`,
`profiles.rs:883`). So in practice user profiles occupy **101–4095**, and **2–100 is
never auto-assigned** (only reachable by an explicit manual pick).

**The concern.** The management VLAN carries RADIUS (1812/1813) and inter-router
RPC. If its id ever collided with a profile VLAN, management traffic would share a
user profile's lane — a correctness *and* security failure. Two constraints follow:
(1) it must be reserved so no profile can take it, and (2) because the management
VLAN is **one flat L2 segment shared by every router** (§3c), **every router must
agree on the same number** — so it cannot be independently auto-allocated per
router; it is Core-decided and propagated (the pairing token already carries
`mgmt_vlan`).

**Options considered:**

| Option | Pros | Cons |
|---|---|---|
| **A. Fixed low reserved tag (2–100)** | below the `101` auto-floor → never auto-collides; simple constant | a manual profile pick in 2–100 could still collide unless also blocked |
| **B. Fixed high tag (e.g. 4094)** | far from the `101` floor | 4095 is 802.1Q-reserved; auto-allocation climbs toward it; still needs a guard |
| **C. Reserved constant fed through the existing collision guard** | one source of truth; reuses `existing_tags` logic; can't be taken or deleted | must be created at init + protected; must be identical network-wide |
| **D. Admin-configurable with validation** | flexible for odd environments | another knob; must propagate to every router (pairing already carries it) |

**Decision: C + D — a fixed constant default that the Core can override in the UI,
with validation, propagated via pairing.** Reserve a single value (default **`4090`**)
that is (a) **added to the collision guard** so `profiles.create`/`set` reject it and
auto-allocation skips it, (b) **carried in the pairing token** (`mgmt_vlan`) so every
router in the network uses the identical id, and (c) not on VLAN `1` (the untagged
LAN default). Making it a reserved value rather than an auto-allocated one is
deliberate: a shared L2 segment needs a network-wide agreed number, and
Core-decides-then-propagates is the only way to guarantee that.

**Core-configurable (DECIDED).** The management VLAN id is a **Core-only setting on
the "Network Authentication" page** (§6), defaulting to `4090`. On change, `dot1x.set`
**validates** the new id (in `2..4094`, not `1`, not colliding with any existing
profile VLAN) and, because the value is network-wide, it can only be changed with a
clear constraint: **all currently-paired Satellites must re-receive it**. Simplest
safe rule — allow free change **before any Satellite is paired**; once Satellites
exist, changing it either (i) is blocked with a message to do it before pairing, or
(ii) triggers a re-propagation to every Satellite over the management VLAN. Given how
rarely this needs changing, **(i) block-after-pairing** is the recommended first
implementation; re-propagation (ii) is a later enhancement. Satellites never choose
their own value — they only receive it via the token.

---

## 6. Frontend Design (Angular, `web/`)

Cross-frontend rule (`AGENTS.md`): every handler change updates
`web/src/app/services/api/api.service.ts` + `live-api.service.ts` +
`mock-api.service.ts` + `API_CONTRACT.md` together.

**Placement: a dedicated "Network Authentication" page (DECIDED).** The role
selector, Satellite pairing/registry, and live status live on one new page; the
per-port mode toggle stays inline on the existing Ethernet page (where ports
already are). Rationale and rejected alternative in §12-I.

- **Role selector** — device-level Core/Satellite toggle, on the new page.
- **Management VLAN id** (Core only) — a numeric field defaulting to `4090`,
  validated on save (`2..4094`, not `1`, no collision with a profile VLAN); editable
  before any Satellite is paired, otherwise locked with an explanatory message
  (§5b).
- **Per-port control** — on the existing Ethernet / Points of Entry page, a
  per-LAN-port mode (Static / 802.1X-client / Satellite-uplink). Selecting
  802.1X-client **requires** a guest-profile pick before the form can save (§4c).
- **No new credential field** — the existing per-profile Wi-Fi password *is* the
  802.1X credential (§4b); the profile editor may just relabel it (e.g. "network
  password") to signal it now covers wired too.
- **Satellite pairing** — on the new page: "Add Satellite" (pick upstream router +
  port → show token/QR), a registry list of paired Satellites with status, and
  remove/revoke. The joining device's "Join as Satellite" token entry also lives
  here (§5a).
- **Status view** — live per-port authenticated-client list + Satellite health from
  `dot1x.status` / `dot1x.satellites`, a recent auth-events panel (successes and
  failures with reason), **and a log viewer backed by `dot1x.logs`** so an admin
  sees *why* a device failed entirely in the UI — no SSH. This is the UI answer to
  "RADIUS internals an admin cares about" (§1 non-goals).

Scope only — component code is out of scope for this design.

---

## 7. Build & Packaging

| Change | File | Detail |
|---|---|---|
| **Package swap** | `build/openwrt.diffconfig:66` | `CONFIG_PACKAGE_hostapd-basic-mbedtls=y` → `CONFIG_PACKAGE_hostapd-mbedtls=y` |
| Stage dirs | `build/stage-files.sh` | ensure `/etc/radius/` exists. No dot1x hotplug/action scripts are staged — the uplink is a static trunk (§3d), so there is no supplicant to start or DHCP to sequence |
| Keep list | `build/stage-files.sh` keep.d | **no change** — pairing state lives in UCI (`/etc/config/*`, kept by default); `/etc/radius/*` is regenerated from UCI at boot, so it must **not** be added to keep.d (§5 Persistence) |
| CI paths mirror | `.github/workflows/start-wrt.yaml` | mirror any new `build.mk`/path inputs (root AGENTS.md "Coupled changes") |

> **Validated:** `hostapd-mbedtls` is `VARIANT:=full-mbedtls` → gets
> `Install/hostapd/full` (the `hostapd-radius` symlink + radius init/config/users/
> clients) **and** compiles with `CONFIG_FULL_DYNAMIC_VLAN=y` + `CONFIG_RADIUS_SERVER=y`.
> `hostapd-basic-mbedtls` (current default) has all three **disabled** — hence the
> swap is required *and* sufficient. The uplink is a static trunk (§3d), so
> **hostapd (authenticator) is the only 802.1X component** — on both Core and
> Satellite — and **no wired supplicant is used**; no supplicant-side package
> change is needed either way. `ip-bridge` already present.

**Image size:** swapping basic→full hostapd increases the hostapd binary; net
image impact expected small but should be measured on the first image build.

---

## 8. API Contract additions (`API_CONTRACT.md`)

- New `dot1x` group: `dot1x.get`, `dot1x.set`, `dot1x.status`, `dot1x.logs`,
  plus the pairing surface `dot1x.satellite-token`,
  `dot1x.satellite-enroll`, `dot1x.satellite-join`, `dot1x.satellites`,
  `dot1x.satellite-remove`, `dot1x.satellite-replace` (§5a). Document the
  `Dot1xConfig` / `PortAuthMode` / `Dot1xRole` shapes and the `config satellite`
  registry entry.
- `profiles.*`: **no new credential field** — the 802.1X user DB is derived from
  the existing profile passwords (Wi-Fi `WifiStation` set), see §4b.
- `ethernet.*`: add `auth_mode` (`Static` / `Dot1xClient{guest}` /
  `SatelliteUplink`) to the per-port object.

---

## 9. Security Considerations

- **RADIUS shared secret / Satellite pairing.** Satellites send Access-Requests
  to the Core; the `radius.clients` file (`<CIDR> <secret>`) authorizes them. The
  default shipped secret is literally `radius` on `0.0.0.0/0` — **must not** ship
  as-is; StartWRT never writes that entry. Each Satellite gets its **own**
  per-Satellite secret provisioned during pairing (§5a), scoped to its
  management-VLAN `/32`. **Rotation** is a pairing action (re-issue secret / re-pair),
  not an editable field.
- **RADIUS exposure.** The Core RADIUS listens on 1812/1813 **bound to the
  management VLAN only**, firewalled from every client profile and WAN. The
  management VLAN is the flat L2 segment of §3c; only StartWRT routers sit on it.
- **Trunk link security (MACsec) — accepted risk, future enhancement.** A
  `SatelliteUplink` trunk trusts VLAN tags on the cable, so a physical attacker on
  that specific cable could VLAN-hop into any profile (§3d). We accept this at the
  same level as existing LAN cabling and rely on physical security of inter-router
  runs. MACsec (802.1AE) would cryptographically bind the trunk to the paired
  Satellite and close it; it is out of scope now but a clean later addition (K1 MAC
  + mainline kernel support it).
- **Credential storage.** Profile credentials live in UCI (`config profile`) and
  the generated `/etc/radius/users` — plaintext on the router, like existing
  Wi-Fi PSKs. Consider storing the NT `hash` instead of `password` in
  `/etc/radius/users` (the parser accepts `hash`+`salt`) so the cleartext isn't
  duplicated. ⟨DECISION NEEDED⟩.
- **PEAP trust.** Clients validate the RADIUS server cert against the StartWRT CA.
  Users who haven't trusted the CA (see `docs/src/trust-ca.md`) will get a
  warning — same UX as the web interface.
- **Dumb-switch limitation** (restated): port-level, not device-level. A
  malicious device behind an authorized switch port inherits access.

---

## 10. Documentation Impact

- New user page `docs/src/wired-dot1x.md` (content derived from
  `FEATURE_SUMMARY.md`), linked in `docs/src/SUMMARY.md` — likely under **Points
  of Entry** (near `ethernet.md`) or a new **Network Authentication** area.
- Cross-reference from `ethernet.md`, `security-profiles.md`, and `trust-ca.md`.
- `CHANGELOG.md` entry for start-wrt (root AGENTS.md: code + changelog land
  together).

---

## 11. Testing & Verification

**Host build / unit:**
- `cargo build -p startwrt-core --bin startwrt`
- `cargo test -p startwrt-core -p uciedit -p uciedit_macros`
- New unit tests: `regenerate_radius_users` JSON output; `RadiusConfig`
  round-trip in `uciedit/src/tests.rs`; `Port` auth_mode serde.

**Image (needs a build host — UNVALIDATED cross-build per AGENTS.md):**
- `make startwrt-image`; confirm image includes `hostapd-radius`, hostapd-full;
  measure size delta. (No wired supplicant is needed — the uplink is a static
  trunk, §3d — so `wpa_supplicant-full` is not required for this feature.)

**Hardware end-to-end (BPI-F3):**
1. Core: enable 802.1X on a LAN port; a client with a valid credential lands in
   the credential's profile subnet; verify the VLAN sub-iface appears on `br-lan`.
2. Client with no 802.1X → default/guest profile (validates the fallback path —
   **top functional risk**, §12-A).
3. Satellite: uplink trunk comes up statically at boot (no supplicant, no WAN
   auth); the management VLAN reaches the Core RADIUS with no DHCP race.
4. Satellite LAN client authenticates against the Core RADIUS; correct profile.
5. Daisy-chain: Core → Satellite → Satellite; RADIUS reaches the Core; profiles
   correct at the far end.
6. `dot1x.status` reflects live per-port client state via wired ubus.

---

## 12. Decisions & Open Risks

Design decisions raised in review are logged here as **DECIDED** for traceability.
The genuinely open items are the ones that can only be settled on hardware:
**A** (dynamic VLAN on `driver=wired`), **B** (wired unauth fallback mechanism),
the runtime risks inside **E** (port bootstrap state machine, multi-hop management
VLAN), and **H** (image size). Ordered by impact.

**A. hostapd wired-driver dynamic VLAN on a software bridge — TOP RUNTIME RISK.**
The upstream `driver_wired.c` / `vlan_full.c` are fetched from git at build time
and can't be inspected in-repo. Dynamic VLAN assignment with `driver=wired`
(vs. the well-trodden nl80211 path) is uncommon and unproven on BPI-F3. If
hostapd-wired won't move an authenticated port to the RADIUS-returned VLAN via
`vlan_bridge`, the whole authenticator model needs rework (fallback: static-VLAN
+ port authorize/deauthorize only, no per-credential VLAN on wired). **Must be
validated on hardware before committing to the VLAN-on-wired approach.**

**B. Unauthenticated fallback (guest VLAN) mechanism on wired — REQUIRED, needs
validation.** How does a device that sends *no* EAPOL (or fails) end up in the
guest VLAN? On wired hostapd this needs a guest-VLAN config, and its behavior
differs from Wi-Fi. This underpins the "never stranded" promise for
ordinary clients (§4c). (It no longer bears on the Satellite uplink — that is a
static trunk with no upstream auth, §12-C.) It is the substrate a future MAB would extend (§1). Must be validated on
hardware; if hostapd-wired can't grant a guest VLAN on failure, the fallback path
needs rework.

**C. The Satellite trust model — DECIDED: trusted VLAN trunk (§3d).** A Satellite
is an *infrastructure peer* (paired, trunk uplink); the Core LAN port has a
three-way mode (Static / 802.1X-client / Satellite-uplink); the guest profile is a
per-port field on 802.1X-client ports. The sub-decisions below (D, E, F) are the
mechanics this choice pulls in.

**Uplink 802.1X (the "Satellite = supplicant on WAN" option) was evaluated and
rejected.** Running per-frame 802.1X on the inter-router link would add a WAN
supplicant daemon, the deferred-DHCP action-script machinery (§5), uplink
boot-order coupling, and a supplicant-credential distribution path — and would
gate on the unproven wired-hostapd behaviors in risks A/B — all to resist a single
attack (a live device-swap on that one trunk cable) that physical security of an
in-premises inter-router run already covers, and that MACsec (§12-J) will later
close cryptographically. Ordinary client ports keep their normal
reset-on-link-down 802.1X regardless; only the inter-router trunk is exempt.

**D. Guest/default profile — DECIDED: explicit, required, per-port.** Each
`Dot1xClient` port carries its own `guest` profile; the admin must choose it
(no silent inheritance of the prior static profile). Enforced in `ethernet.set`
and required in the UI before save (§4c, §6).

**E. Satellite pairing / RADIUS trust bootstrap — SPECIFIED inline (§5a).**
Automated, Core-authoritative enrollment; token+CA-fingerprint breaks the
cert chicken-and-egg; per-Satellite secrets; central registry; hub-and-spoke and
daisy-chain both covered (N Satellites, downstream enrollment driven by the Core
with the intermediate as relay). Residual small choices (token format, bootstrap
timeout, management-VLAN id) are listed in §5a and deferrable to implementation.
**Hardware/runtime risks within pairing:** the temporary management-only→trunk
port state machine on the upstream router, and management-VLAN reachability across
a multi-hop chain, need on-hardware validation.

**F. RADIUS transport across a daisy-chain — RESOLVED (§3c).** A single flat L2
management VLAN spans the whole chain, bridged (not routed/proxied) across every
trunk; all routers share one management subnet and address the Core's RADIUS
directly. Residual: confirm on hardware that multi-hop L2 bridging of the
management VLAN behaves (part of §E's runtime validation).

**G. Credential model — DECIDED (reuse Wi-Fi password, §4b); username uniqueness
RESOLVED.** Verified in code: `wifi.rs:628` already rejects duplicate non-empty
password labels (`DuplicatePasswordLabel`), and empty labels fall back to the
profile name on read (`wifi.rs:221`). So labels are unique enough to use as RADIUS
usernames. The one residual edge — two empty-label passwords under the *same*
profile both falling back to the profile name — is handled in
`regenerate_radius_users` by disambiguating (`<profile>` / `<profile>-2` …). No
schema or UX change needed.

**H. Image size / flash budget** from the hostapd basic→full swap — measure.

**I. UI placement — DECIDED: dedicated "Network Authentication" page.** Role,
pairing/registry, and status on one new page; per-port mode stays on the Ethernet
page. (Rejected alternative — distributing across Settings/Ethernet/Devices —
scattered the feature and left pairing homeless.)

**J. MACsec (802.1AE) on the Satellite trunk — deferred to a phase-2 workstream,
not in this PR.** Assessed against the OpenWrt tree; it is a substantial add, not a
toggle:

- *Easy parts.* Kernel: `kmod-macsec` exists (`CONFIG_MACSEC`, needs
  `kmod-crypto-gcm`) — one diffconfig line. Tooling: `ip-full` already carries
  `ip macsec`.
- *The blocker.* Every wpa_supplicant variant, **including `-full`, ships with
  `CONFIG_MACSEC` and `CONFIG_DRIVER_MACSEC_LINUX` commented out**
  (`wpa_supplicant-full.config:186,80`), and those `.config` files are copied
  verbatim from **inside the `openwrt/` submodule** at build
  (`hostapd/Makefile:593`). Enabling MKA therefore needs an **upstream change to
  Start9's fork** (enable MACsec in the mbedtls variant, or add a new variant) or a
  local package-override feed — it is *not* reachable from `openwrt.diffconfig`, and
  editing the submodule is disallowed here. This breaks the "single PR, no submodule
  change" property that the rest of the feature holds.
- *Crypto-backend risk.* MKA needs AES-CMAC + AES-Key-Wrap; whether the **mbedtls**
  wpa_supplicant backend provides them for MKA is unverified (may force a wolfssl/
  openssl variant → larger image).
- *Topology work.* A trunk port becomes a `macsec0` virtual netdev over the
  physical port, and `br-lan` + all profile VLANs must ride on `macsec0` instead of
  the raw port — a non-trivial change to how §3d writes trunk ports.
- *Key management.* Provision a pre-shared CAK/CKN to both ends during pairing
  (extend the §5a bundle) and run wpa_supplicant in MKA mode (or static SAs via
  `ip macsec`) on the trunk.
- *Runtime cost.* No known K1 hardware offload → software AES-GCM per frame on the
  trunk; measure throughput impact.

Net: MACsec is a coherent **follow-up feature** with its own upstream dependency and
hardware validation, best done after the base feature is proven. The base design is
forward-compatible — it changes only how trunk ports are plumbed, not the trust or
pairing model.
