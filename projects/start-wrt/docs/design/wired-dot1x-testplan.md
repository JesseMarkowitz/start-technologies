# Wired 802.1X — Test & Validation Plan

Companion to the design doc [`wired-dot1x.md`](wired-dot1x.md). This document
enumerates the scenarios that validate wired 802.1X, organised in three layers by
what they need to run:

1. **Automated unit tests** — run in CI on every change; no device.
2. **CLI / config-level functional tests** — a dev host, the `startwrt` binary, and
   a scratch UCI tree; no device. Validates the config read/write/derivation logic
   that has landed.
3. **On-device end-to-end (BPI-F3)** — real hardware; validates the runtime
   (hostapd authenticator, RADIUS auth, VLAN assignment, Satellite pairing) that is
   **deferred** to a later slice (see the design doc §4 "Implementation status").

Each scenario states **setup → action → expected result**. A scenario passes only
if the observed result matches exactly.

> **Status legend:** ✅ implemented & validated · 🟡 implemented, validate on device
> · ⛔ blocked on deferred runtime/frontend wiring.

---

## 0. Test environment

**Backend (dev host):**

```
# Build the binary (lands in the workspace-root target/)
cargo build -p startwrt-core --bin startwrt

# Run the whole backend unit suite
cargo test -p startwrt-core -p uciedit -p uciedit_macros
```

The CLI runs handlers **locally** against a config directory:

- `--config-root <dir>` points at a UCI tree (default `/etc/config`).
- `--configs-only` makes the run **non-effectful** — it writes UCI but skips all
  service reloads (`radius`/hostapd/`wifi`) and the derived-file writes. Use it for
  every dev-host test so nothing touches the real system.

> **Dev-host prerequisite for write commands.** Any handler that writes (here
> `dot1x set`, also `ethernet set`, `profiles *`) appends to the activity log at
> `/etc/startwrt/activity.db`, and panics if that directory is missing. Create it
> once: `sudo mkdir -p /etc/startwrt && sudo chown "$USER" /etc/startwrt`. Read-only
> commands (`dot1x get`) need nothing. This is a shared StartWRT prerequisite, not
> specific to 802.1X.

**Scratch UCI tree** used by the §2 scenarios (two profiles: Admin/VLAN 1,
Guest/VLAN 3; one admin Wi-Fi password and one Guest-bound password):

```
# $D is a fresh scratch dir
cat > "$D/startwrt" <<'EOF'
config profile lan
	option fullname 'Admin'
	option interface 'lan'
	option vlan_tag '1'

config profile guest
	option fullname 'Guest'
	option interface 'guest'
	option vlan_tag '3'
EOF
cat > "$D/wireless" <<'EOF'
config wifi-station
	option key 'adminpass'

config wifi-station
	option key 'guestpass'
	option vid '3'
	option label 'Guest'
EOF
: > "$D/radius"
```

---

## 1. Automated unit tests ✅

Run: `cargo test -p startwrt-core -p uciedit -p uciedit_macros` (445 + 17 pass).
The 802.1X-specific tests:

| Test | Validates |
|---|---|
| `dot1x::tests::get_defaults_to_disabled_when_no_dot1x_section` | Un-provisioned router reports 802.1X **off** (regression guard, see §2.1) |
| `dot1x::tests::radius_users_derives_from_profile_passwords` | `/etc/radius/users` JSON: profile passwords → PEAP/MSCHAPV2 users with `vlan-id`; admin password excluded |
| `dot1x::tests::set_then_get_round_trips_config` | `set` persists global + per-port modes + flips the `radius` enable bit; `get` reads them back |
| `dot1x::tests::set_rejects_unknown_guest_profile` | A `dot1xClient` port with a guest that resolves to no profile is rejected |
| `dot1x::tests::parse_role_defaults_to_core` | Role string mapping, unknown → Core |
| `ethernet::tests::get_overlays_persisted_dot1x_auth_mode` | `ethernet.get` surfaces persisted `dot1x_port` modes on `Port.auth_mode` |

**Pass criteria:** all green, exit 0.

---

## 2. CLI / config-level functional tests (dev host, no device)

These exercise the landed backend against the §0 scratch tree.

### 2.1 Un-provisioned router reports 802.1X off ✅

- **Setup:** scratch tree with **no** `config dot1x` section.
- **Action:** `startwrt --config-root "$D" --configs-only dot1x get`
- **Expect:**
  ```json
  { "enabled": false, "role": "core", "ports": {} }
  ```
- **Why it matters:** a fresh router must default to OFF. (Earlier the absent
  section read as `enabled: true` — fixed and guarded by the unit test above.)

### 2.2 Enable a Core with mixed port modes ✅

- **Setup:** §0 scratch tree.
- **Action:** pipe the desired config to `dot1x set`:
  ```
  echo '{"enabled":true,"role":"core","ports":{
          "eth0":{"mode":"static"},
          "eth1":{"mode":"dot1xClient","guest":{"fullname":"Guest","interface":"guest","vlan_tag":3}}}}' \
    | startwrt --config-root "$D" --configs-only dot1x set
  ```
- **Expect:** `dot1x get` afterward returns the same config — `eth0` `static`,
  `eth1` `dot1xClient` with `guest.vlan_tag: 3`, `enabled: true`.
- **Inspect UCI:** `$D/startwrt` now contains a `config dot1x 'dot1x'` section
  (`option disabled '0'`, `option role 'core'`) and a `config dot1x_port 'eth1'`
  (`option mode 'dot1xClient'`, `option guest_vlan '3'`); `$D/radius` has a
  `config radius 'radius'` with `option disabled '0'` and `option users
  '/etc/radius/users'`.

### 2.3 `ethernet.get` reflects the persisted port mode ✅

- **Setup:** after 2.2 (needs `$D/network` with a `br-lan` bridge over `eth0`/`eth1`
  — see the `ethernet` tests for a minimal one).
- **Action:** `startwrt --config-root "$D" --configs-only ethernet get`
- **Expect:** `ports.eth1.auth_mode.mode == "dot1xClient"`; `ports.eth0.auth_mode.mode == "static"`.

### 2.4 Reject a `dot1xClient` port with an unknown guest profile ✅ (unit)

- **Action:** `dot1x set` with a port whose `guest` names a nonexistent profile
  (e.g. `vlan_tag: 4000`).
- **Expect:** non-zero exit, nothing persisted. (Covered by
  `set_rejects_unknown_guest_profile`; the CLI path also errors but requires the
  activity-DB prerequisite from §0 to reach the log line.)

### 2.5 Disabling a Core turns the RADIUS server off ✅

- **Action:** `dot1x set` with `{"enabled":false,"role":"core","ports":{}}`.
- **Expect:** `dot1x get` → `enabled: false`; `$D/radius` `radius` section shows
  `option disabled '1'`.

### 2.6 RADIUS user DB derivation ✅ (unit; effectful on device)

- Validated by `radius_users_derives_from_profile_passwords`: the Guest-bound
  password becomes one PEAP/phase2 user keyed by its label, `vlan-id 3`,
  method `MSCHAPV2`; the admin password (no VLAN) is excluded; the outer method is
  `PEAP`. On a device (effectful) this is what `write_radius_users` writes to
  `/etc/radius/users` (mode 0600) on `dot1x set` and on any `profiles.*` change.

---

## 3. On-device end-to-end (BPI-F3) 🟡/⛔

These require the deferred runtime wiring (per-port hostapd-wired authenticator,
Satellite pairing, boot regeneration). They also settle the design's open hardware
risks (§12 A/B/E). **Do not mark 802.1X "working" until these pass on hardware.**

### 3.1 Wired client authenticates → credential's profile ⛔ (design §12-A, top risk)

- **Setup:** Core with `dot1x` enabled; a LAN port set to `dot1xClient` (guest =
  Guest); a client configured with a profile's Wi-Fi password as its 802.1X
  credential (PEAP/MSCHAPv2).
- **Expect:** client lands in that credential's profile subnet; the profile's VLAN
  sub-interface appears on `br-lan`; `hostapd-wired` moved the port to the
  RADIUS-returned VLAN. **Fails ⇒ the VLAN-on-wired authenticator model needs
  rework (§12-A).**

### 3.2 Unauthenticated / failing client → guest profile ⛔ (design §12-B)

- **Setup:** as 3.1; connect a device that sends no EAPOL or fails auth.
- **Expect:** device gets the port's configured **guest** profile — never stranded.
  **This underpins the "never stranded" promise; validate the wired guest-VLAN
  mechanism explicitly.**

### 3.3 `dot1x.status` reflects live clients 🟡

- **Action:** `startwrt dot1x status` on the Core with authenticated clients.
- **Expect:** each `dot1xClient` port reports `present: true` and the authenticated
  client MACs (from `ubus call hostapd.<port> get_clients`).

### 3.4 `dot1x.logs` surfaces auth events 🟡

- **Action:** `startwrt dot1x logs` after auth attempts.
- **Expect:** recent hostapd/RADIUS lines (accept & reject) — diagnosable without SSH.

### 3.5 Credential sync on profile change 🟡

- **Action:** on an enabled Core, change/add/delete a profile's Wi-Fi password via
  `profiles.*`.
- **Expect:** `/etc/radius/users` regenerates within the same operation; a client
  using the new secret authenticates into the right profile.

### 3.6 Satellite uplink comes up statically at boot ⛔ (design §3d, §5, §12-E)

- **Expect:** the Satellite's uplink trunk (management + profile VLANs) is up as soon
  as the cable links — no wired supplicant, no DHCP race, independent of Core power
  state.

### 3.7 Satellite LAN client → Core RADIUS → correct profile ⛔

- **Expect:** a wired client on a **Satellite** LAN port authenticates against the
  **Core** RADIUS over the management VLAN and lands in the same profile it would on
  the Core.

### 3.8 Daisy-chain (Core → Sat → Sat) ⛔ (design §3c, §12-F)

- **Expect:** RADIUS reaches the Core across the flat management VLAN at every hop;
  profiles correct at the far end; each trunk's L2 converges bottom-up as links power
  on.

### 3.9 Convergence after Core outage ⛔ (design §5)

- **Setup:** Satellite up, Core down (its clients parked in guest); bring the Core up.
- **Expect:** on `core_reachable` false→true the Satellite nudges parked clients to
  re-run EAPOL and they move to their real profiles within **seconds**, not the
  passive multi-minute/1-hour timers.

### 3.10 Backup / preserve-config upgrade ⛔ (design §5)

- **Expect:** UCI (`dot1x`/`dot1x_port`/`radius`/`satellite`) rides the backup and
  survives a keep-config upgrade; `/etc/radius/users` & `clients` are **regenerated
  at boot** (not kept), and auth still works after.

---

## 4. Traceability

| Scenario | Design ref | Layer | Status |
|---|---|---|---|
| 2.1 default-off | §4 impl-status | unit + CLI | ✅ |
| 2.2 enable Core / port modes | §4a, §4c | CLI | ✅ |
| 2.3 ethernet overlay | §4c | CLI | ✅ |
| 2.4 guest required | §4c, §12-D | unit | ✅ |
| 2.5 disable → RADIUS off | §4d | CLI | ✅ |
| 2.6 user DB derivation | §4b | unit | ✅ |
| 3.1 wired auth → profile | §3a, §12-A | device | ⛔ |
| 3.2 unauth → guest | §4c, §12-B | device | ⛔ |
| 3.3 status | §4a | device | 🟡 |
| 3.4 logs | §4a | device | 🟡 |
| 3.5 credential sync | §4b | device | 🟡 |
| 3.6 Satellite uplink | §3d, §5 | device | ⛔ |
| 3.7 Satellite LAN auth | §3c | device | ⛔ |
| 3.8 daisy-chain | §3c, §12-F | device | ⛔ |
| 3.9 convergence | §5 | device | ⛔ |
| 3.10 backup/upgrade | §5 | device | ⛔ |

---

## 5. Not yet testable (blocked on deferred work)

The following can't be exercised until the deferred slice lands (design §4
"Implementation status"): the per-port **hostapd-wired authenticator** config +
reload, **Satellite pairing/enrollment** and `/etc/radius/clients`, **boot-time**
user-DB regeneration, `firstboot_config/radius` staging, and the **Angular
frontend**. Until then, §3 is specification-only; §1–2 are the live coverage.
