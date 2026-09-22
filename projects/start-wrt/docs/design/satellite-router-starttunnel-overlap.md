# StartWRT and StartTunnel — what overlaps, and what the satellite work should take

> **Superseded by the filed issue (2026-09-21).** Research note. Its conclusions are folded into the issue attachment (`SupportingEvidenceForSatelliteRouter.md`, §1 alternative 7 and §2 concern 7). Kept for the detail behind them; the attachment is authoritative.

Working note. Explains what StartTunnel is, why a satellite router is structurally the same shape,
what already exists there that the satellite design proposes to build from scratch, and what should
_not_ be borrowed. Code references are against `master` as of 2026-09-20.

---

## 1. What StartTunnel is

StartTunnel is Start9's **VPS-hosted virtual private router**. You run it on a rented server with a
public IP; your StartOS box (and anything else you enroll) dials in over WireGuard; the VPS forwards
clearnet traffic to it. It exists to give a self-hoster a public address when their ISP will not —
CGNAT, no static IP, blocked ports.

The product directory `projects/start-tunnel/` is a thin wrapper. The substance lives in
`shared-libs/crates/start-core/src/tunnel/` — the same crate StartWRT links. From
`projects/start-tunnel/ARCHITECTURE.md`:

| Concern               | Location                                                                                            |
| --------------------- | --------------------------------------------------------------------------------------------------- |
| Daemon + CLI dispatch | `start-core/src/bins/tunnel.rs`                                                                     |
| State model (PatchDB) | `start-core/src/tunnel/db.rs`                                                                       |
| JSON-RPC API          | `start-core/src/tunnel/api.rs`                                                                      |
| WireGuard control     | `start-core/src/tunnel/wg.rs`, `wg6.rs`                                                             |
| Port-forward engine   | `start-core/src/tunnel/forward/` (`mod.rs`, `pcp.rs`, `igd.rs`, `sni.rs`, `pinhole.rs`, `lease.rs`) |
| Per-subnet DNS        | `start-core/src/tunnel/dns.rs`                                                                      |

---

## 2. Why a satellite is the same shape

Strip away the purpose and both products are the same object: **a WireGuard hub that terminates
tunnels from remote segments, attaches each to a policy, and performs egress and inbound forwarding
on their behalf.**

|                   | StartTunnel                                  | StartWRT Core with satellites                            |
| ----------------- | -------------------------------------------- | -------------------------------------------------------- |
| Hub               | VPS with a public IP                         | Core router with the only WAN                            |
| Spokes            | StartOS boxes and other clients              | Satellite routers                                        |
| What a spoke is   | one host, one tunnel IP                      | one _router_, carrying whole subnets                     |
| Direction of need | the spoke needs the hub's **public address** | the spoke needs the hub's **Internet uplink and policy** |
| Unit of policy    | a subnet (`WgSubnetConfig`)                  | a Security Profile                                       |
| Inbound           | the hub owns external ports and DNATs inward | the Core owns external ports and DNATs inward            |

The difference that matters: **StartTunnel's peers are hosts, StartWRT's satellites are routers.** A
StartTunnel client gets a `/32` in the hub's subnet. A satellite advertises a whole `/24` per profile
and serves DHCP behind it. That is a real distinction — it is precisely why the design calls for a
new `vpn_site.rs` rather than overloading `vpn_server.rs`'s host-peer path — but it is a difference
in the _peer model_, not in the surrounding machinery.

---

## 3. What already exists there that the satellite design proposes to build

Audited against `start-core/src/tunnel/` and #3682's own inventory.

### 3.1 Subnets as the unit of policy — `db.rs`, `wg.rs`

```rust
pub struct WgSubnetConfig {
    pub name: InternedString,
    pub clients: WgSubnetClients,       // BTreeMap<Ipv4Addr, WgConfig>
    pub dns: DnsConfig,                 // per-subnet resolver selection
    pub wan_ip: Option<Ipv4Addr>,       // per-subnet egress SNAT
    pub ipv6: Option<Ipv6Net>,          // routed prefix delegated to this subnet
}
```

This is, structurally, a Security Profile: a named segment with its own client set, its own DNS
policy and its own egress identity. The satellite design's "routed attachment, per-router subnets"
and "per-profile DNS on the satellite" are both already modelled here — and `DnsConfig` is
per-subnet, which is exactly the shape concern 9 needs.

**Worth studying, not copying wholesale.** StartWRT's profile is richer (VLAN tag, firewall zone,
schedules, VPN chaining) and lives in UCI, not PatchDB. But the _decomposition_ — a segment carries
its own DNS and egress rather than inheriting a global one — is settled here and was reached
independently in the satellite design. That is corroboration worth citing.

### 3.2 A routed IPv6 prefix per subnet — `wg6.rs`

This is the most valuable find, and it bears directly on the design's most-gated item.

The satellite design says DHCPv6-PD over a WireGuard interface has no precedent in this codebase and
gates the whole IPv6 phase on a bench spike. That is true of _DHCPv6-PD_. It is not true of the
underlying problem. StartTunnel already delegates a routed IPv6 prefix to a segment reached over
WireGuard, and does it **without any delegation protocol at all**:

```rust
/// The IPv6 address for a host whose tunnel IPv4 is `v4`, on a subnet whose
/// delegated prefix is `prefix`: the prefix's network bits OR'd with the tunnel
/// IPv4 clamped to the prefix's host space.
pub fn host_v6(prefix: Ipv6Net, v4: Ipv4Addr) -> Ipv6Addr
```

Every host's `/128` is _derived_ from its tunnel IPv4, so addresses are stable and computable with no
allocation state — the UI can show a device's IPv6 with no backend round-trip. `v6_conflict` and
`first_v6_collision` reject a prefix too small to give every host a distinct address.

For the satellite work this is the **fallback path already written**: the Core allocates a `/64` per
profile per satellite and pushes it as delegated state, the satellite carves host addresses
deterministically. It de-risks IPv6 considerably — the spike becomes "is DHCPv6-PD _nicer_ than what
StartTunnel already does," not "is IPv6 possible at all."

One caveat to state honestly: #3682 lists "the derivable per-device IPv6 address scheme" under **Not
porting (StartTunnel-specific)**. That decision is about StartWRT's _LAN devices_, which get addresses
from SLAAC/DHCPv6 on a real broadcast segment and should not be renumbered into a derived scheme. A
satellite's _transit_ addressing is a different question and the exclusion should not be read as
covering it. Worth confirming with the maintainer rather than assuming either way.

### 3.3 The port-forwarding engine — `forward/`

The satellite design's D12 (satellite relays, Core authorizes) proposes building a PCP/UPnP terminator
on the satellite and an authorization path at the Core. StartTunnel has the whole engine:

- `forward/pcp.rs` — PCP server, including PORT_SET and ANNOUNCE.
- `forward/igd.rs` — IGD/UPnP, including the vendor hostname actions.
- `forward/lease.rs` — lease bookkeeping and expiry.
- `forward/pinhole.rs` — IPv6 pinholes.
- `forward/sni.rs` — SNI-based routing so several targets share port 443.
- `db.rs` — `PortForwards`, `Pinholes6`, `SniRoute`, `HttpRedirects`, plus `gc_forwards` and
  `overlapping()` collision checks.

StartWRT already shares some of this: #3634 and #3783 landed the PCP/UPnP gateway on the StartWRT
side. The satellite-specific part — _relay a request from a downstream router upward, and authorize
it against a registry_ — is genuinely new. But the protocol termination, the lease model and the
collision rules are not.

### 3.4 Peer authorization by tunnel address and public key

#3682 describes teaching `port_control.rs` "to authorize a WireGuard peer by tunnel address and
public key (today it only resolves a neighbor-table MAC against a DHCP host's flag)."

That is, almost word for word, the substitute check the satellite design proposes in concern 14 for
`arrival_matches` — and it is already on the roadmap for StartWRT for a different reason. The two
should be one piece of work: a single "authorize by tunnel identity rather than neighbor table" path
serving both the StartTunnel-style peer and the satellite.

### 3.5 Live state transport

StartTunnel's UI subscribes to a PatchDB patch stream; StartWRT polls every form on a 5 s timer
(`web/src/app/services/form.service.ts:26-46`). #3681 names this explicitly and flags it as the
biggest lift of the convergence items.

The satellite design needs a Core→satellite push with a **monotonic generation number**, reconcile on
reconnect, and a UI that shows which satellites are stale. That is a patch stream with extra steps.
Whether to adopt PatchDB for this is a genuine fork in the road, and #3681 already says "decide
explicitly whether it's in scope."

---

## 4. What should _not_ be borrowed

- **The host-peer model.** A StartTunnel client is one host with a `/32`. Satellites advertise
  subnets. `vpn_server.rs`'s `allocate_peer_ip`, per-octet route naming and proxy-ARP
  (`vpn_server.rs:802 sync_proxy_arp`) all assume on-link hosts in the profile `/24` and are wrong for
  a routed subnet. The design's call for a separate `vpn_site.rs` stands.
- **PatchDB as the satellite's config store.** StartWRT's state is UCI, and D5's whole point is that
  the satellite regenerates local config through the existing `profiles.rs` chain. Introducing a
  second state model on the satellite would undo that.
- **The subnet-as-hub addressing.** StartTunnel hands out tunnel IPs from the hub's own block.
  StartWRT satellites own their own `/24`s, allocated by the Core but served locally with local DHCP.
  Different problem, different answer.
- **StartTunnel's session auth** (`tunnel/auth.rs`). It is the same generation of hand-rolled auth
  StartWRT has; #3670 is migrating away from that family, not toward it.

---

## 5. How the two products might converge

#3681 ("Refactor: share code with StartTunnel") sets the guiding rule: **move code into
`shared-libs/` rather than cross-importing between products.** Its named targets:

- **WireGuard key handling** — `projects/start-wrt/backend/ctrl/src/wg.rs` is nearly identical to
  `start-core/src/tunnel/wg.rs:163-192`. Lift into a shared module. The satellite work generates
  keypairs and PSKs at pairing, so it is a direct beneficiary.
- **Auth** — tracked as #3670, the largest convergence (see `satellite-router-auth-and-3670.md`).
- **UI components** — the QR dialog pattern is duplicated three ways; `Masked`, help-modal and
  error-toast services are per-app. A satellite pairing flow almost certainly wants a QR/fingerprint
  confirmation dialog, which is that same component.
- **State transport** — patch stream vs polling, explicitly undecided.

#3682 ("Port missing StartTunnel functionality to StartWRT") is the feature-level inventory, audited
2026-09-02. Its relevant conclusions: per-device toggles and the PCP/UPnP gateway are already ported;
subnets-as-hub, per-device egress IP and the derivable per-device IPv6 scheme are listed as _not_
porting; hostname routes, manual DNS records and session listing remain.

**The convergence point for the satellite work is narrower than "merge the products."** It is three
specific things:

1. Shared WireGuard key/PSK handling (#3681) — small, uncontroversial, and satellite pairing uses it.
2. One "authorize a peer by tunnel identity" path serving both the StartTunnel peer case (#3682) and
   the satellite case (concern 14) — medium, and currently scheduled to be written twice.
3. A decision on whether routed-prefix-per-segment IPv6 (`wg6.rs`) is the satellite's v6 mechanism or
   only its fallback — which changes how much of the IPv6 phase is research and how much is reuse.

---

## 6. What to do about it

**In the issue:** one sentence acknowledging StartTunnel, and the `Related:` line naming #3681 and
#3682. A maintainer will think of StartTunnel within two paragraphs of reading the proposal; being
the one to raise it first is worth more than the space it costs.

**Before the IPv6 phase:** read `tunnel/wg6.rs` properly and decide whether it is the mechanism or
the fallback. This may remove a spike from the critical path.

**Before the port-forward relay phase:** read `forward/pcp.rs`, `forward/lease.rs` and `db.rs`'s
`overlapping()` / `gc_forwards`. The lease and collision semantics are solved; only the relay and the
authorization are new.

**Ask the maintainer one question:** is a Core-plus-satellites deployment meant to eventually _be_ a
StartTunnel-style hub with a different spoke type, or are these permanently separate products that
merely share a crate? The answer decides whether the satellite registry should be built in
StartWRT's own shape or against the shared tunnel model — and it is much cheaper to ask now than to
discover after the registry is written.

---

## 7. One-paragraph summary

StartTunnel is a WireGuard hub that terminates tunnels from remote segments, attaches each to a
named policy with its own DNS and egress, and owns inbound port forwarding on their behalf — which is
structurally what a StartWRT Core with satellites is, differing mainly in that its spokes are hosts
rather than routers. It already ships several things the satellite design proposes to build: subnets
as the unit of policy with per-subnet DNS and egress, a routed IPv6 prefix per subnet with
deterministic host addressing that could serve as the satellite's v6 mechanism or its de-risked
fallback, a complete PCP/UPnP/SNI forwarding engine with lease and collision handling, and — already
on StartWRT's roadmap in #3682 — authorization of a WireGuard peer by tunnel address and public key,
which is the same check concern 14 needs. What should not be borrowed is the host-peer addressing
model, PatchDB as the satellite's config store, and StartTunnel's own hand-rolled auth. The practical
convergence is three items: shared WireGuard key handling (#3681), one peer-authorization-by-tunnel
path instead of two, and a decision on `wg6.rs` for IPv6.
