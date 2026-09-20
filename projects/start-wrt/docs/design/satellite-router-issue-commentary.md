# Commentary: how the satellite-router draft compares to the real StartWRT issue corpus

> **Superseded as a critique, retained as the record (2026-09-20).** This was written against the
> first ~22,000-character draft. That draft has since been rewritten to ~5,900 characters in house
> style, and its detail moved to `SupportingEvidenceForSatelliteRouter.md`. The findings in §4–§7
> were acted on: #3670 and StartTunnel now have their own notes
> (`satellite-router-auth-and-3670.md`, `satellite-router-starttunnel-overlap.md`), the six
> collisions are worked through in `satellite-router-issue-collisions.md`, and the issue now carries
> `file:line` anchors, a work checklist and a `Related:` line. The length and style comparisons in
> §3 describe the superseded draft.

Written after reviewing every issue carrying the `StartWRT` label on `Start9Labs/start-technologies`
(28 issues: 27 open, 1 closed, as of 2026-09-20), plus the repo's issue templates, triage workflow,
label taxonomy, and `rfcs/` directory. **The draft was not modified.** This document is about fit,
not about whether the proposal is right.

---

## 1. The corpus, briefly

|                                      |                                                             |
| ------------------------------------ | ----------------------------------------------------------- |
| Total `StartWRT` issues              | 28 (27 open, 1 closed)                                      |
| Authors                              | `Dominion5254` ×25, `helix-nine` ×2, `dr-bonez` ×1          |
| Outside-contributor feature requests | **zero**                                                    |
| Body length: median                  | ~1,600 characters                                           |
| Body length: largest                 | **6,977** (#3682, _Port missing StartTunnel functionality_) |
| Bodies over 5,000 chars              | 4 (#3682, #3670, #3662, #3672)                              |
| Draft under review                   | **~22,000 characters**                                      |

The entire StartWRT tracker is maintainers writing to each other. That single fact colours every
comparison below: the house style is optimized for readers who already know the codebase, and it has
never had to absorb a proposal from someone outside the team.

---

## 2. Where the draft matches the house style

**Problem-first framing.** The corpus consistently leads with the defect or the gap, not the
solution. #3676 opens "Single-WAN is hard-coded throughout" and lists where. #3673 names the leak
before proposing the fix. The draft's structure — problem, then table of inadequate workarounds,
then proposal — is the same instinct, and the feature-request template explicitly asks for it ("Lead
with the problem").

**Decisions-before-code callouts.** This is genuinely a house convention, and the draft has it. #3682
writes "**Decision first:** is StartWRT meant to be a CGNAT escape hatch at all?" and "Design
decision before code." #3670 writes "**Blocker to verify first:**". The draft's 23 numbered concerns
are the same move, at larger scale.

**Honest status reporting.** #3682 states exactly what it audited and when ("Audited 2026-09-02
against master plus the open StartWRT PRs (#3783 and #3888)"). The draft's "**Nothing has been run on
two physical routers yet**" is the same discipline, and it is the draft's strongest credibility move
— the corpus visibly rewards people who mark the boundary of what they've verified.

**Naming affected files.** Every substantive issue in the corpus does this. The draft's "Affected
areas" section does too, and at roughly the right granularity.

**Explicit non-goals.** #3682 has a "Not porting (StartTunnel-specific)" section. The draft's
non-goals list is the same device, used the same way — to stop a discussion from sprawling.

---

## 3. Where the draft departs from the house style

### 3.1 It is three times longer than anything in the tracker

The largest StartWRT issue is 6,977 characters and it is a _tracking issue for an entire subsystem
port_. The draft is ~22,000. Median issue is ~1,600.

This is not automatically wrong — the proposal is bigger than anything in the tracker — but it means
the draft will not be read the way the corpus is read. #3676 is scannable in ninety seconds. The
draft is a document, and the concerns list alone is longer than the longest existing issue.

### 3.2 It cites files but almost never lines

This is the sharpest stylistic gap. The corpus anchors nearly every claim to `file.rs:line`:

> `backend/ctrl/src/wan.rs:23` — `WAN_INTERFACE = "wan"` (#3676)
> `middleware/auth.rs:144` — `if metadata.no_auth || self.is_loopback` (#3670)
> `published_ports.rs:1147` — emitted with `reflection '0'` (#3682)

The draft makes structurally identical claims — "zone membership in `profiles.rs` is keyed on
**ingress interface name**, not on subnet"; "the WireGuard accept rule's source zone is hardcoded
`wan`"; "`arrival_matches` compares the arrival interface index against…" — but gives no line
references. Those facts _were_ verified against the code during the design work; the draft just
doesn't show its work in the format this tracker uses. To a maintainer, an unanchored claim about
their own code reads as an assertion; an anchored one reads as a finding.

### 3.3 It uses the template's field names; the corpus doesn't

Every existing StartWRT issue is free-form with its own headings (`## Goal`, `## Background`,
`## Why`, `## Work`, `## Sequencing notes`, `## Related / out of scope`). None uses the feature
template's "Problem & use case / Proposed solution / Alternatives considered / Affected areas /
Anything else."

That is because the template exists for outside contributors and the corpus has none. The draft
following the template is defensible — arguably correct for a first-time external proposal — but it
will look visibly different from its neighbours.

**Mechanical consequence worth knowing:** `.github/workflows/issue-triage.yml` applies the project
label by parsing a `### Project` heading out of the body. If the draft is filed as a _blank_ issue in
the template's prose shape but without that exact heading, it lands unlabeled and gets tagged
`needs-triage`. Also: the workflow assigns anything with type `Feature` to **MattDHill**, not to
`dominion5254` (the StartWRT owner). A feature-typed issue therefore routes away from the StartWRT
maintainer by design.

### 3.4 It argues; the corpus asserts

The corpus is flat and declarative. No persuasion, because the author already has authority over the
roadmap. The draft has a "Why this belongs in StartWRT rather than around it" section, a table of
user workarounds, a prior-art paragraph positioning the gap, an "Offer," and a closing invitation to
be told it's the wrong shape.

For an outside proposal of a large feature this is reasonable and probably necessary. It is also the
most visible tonal difference, and some of it is load-bearing while some is not: the workaround table
is evidence, the closing invitation is etiquette, the prior-art paragraph is positioning.

### 3.5 No `- [ ]` work list

#3670 and #3682 — the two closest analogues in scale — both structure the work as markdown
checkboxes, explicitly so the issue can serve as a tracking issue across several PRs. #3670 says so
outright: "This is a **tracking issue** for a migration that lands as several reviewable PRs."

The draft's ten phases are a numbered list, not checkboxes. The corpus's convention for
multi-PR work is checkboxes, and the draft's shape is exactly multi-PR work.

### 3.6 No "Related: #NNNN" line

Every substantial issue in the corpus ends by cross-referencing its neighbours. #3676 → #3680.
#3682 → #3681, #3784. #3670 → #3665. The draft references no issue numbers at all.

This is the draft's largest _substantive_ omission, and it is worth its own section.

---

## 4. Real collisions the draft does not mention

Seven open issues bear directly on the proposal. Two of them change what should be built.

### 4.1 #3670 — the auth middleware the draft proposes to extend is scheduled for deletion

**This is the most important finding in this commentary.**

The draft's "Affected areas" says: "`middleware/auth.rs` + `auth.rs` — a new remote-peer auth path…
**This is the security-critical change.**" Concerns 11 and 12 build on it.

#3670 is an open, detailed plan to **retire that entire auth stack** — delete the loopback bypass,
replace the browser session cookie with `start-core`'s Ed25519 signature auth, move the local cookie
to `Authorization: Bearer`, and adopt StartOS's OR-composed middleware model. It names
`middleware/auth.rs:144` and `middleware/auth.rs:98/108` as the things going away.

So the draft proposes building a new authentication path into a file whose owner has already written
down a plan to replace it. The right framing is the opposite of what the draft says: a satellite's
per-pairing token should probably be _a fourth middleware in the start-core OR-composition_ that
#3670 is migrating to, not a new branch in the current bespoke stack. That also makes concern 12's
requirement (token bound to the tunnel source, unreachable from the LAN) much easier to state,
because the composition model already exists and is audited.

A revision should cite #3670 and say explicitly whether satellite auth lands before, after, or as
part of it. As written, the draft looks like it hasn't read the tracker.

### 4.2 #3862 — the router doesn't know its real IPv6 PD size

Concern 16 says PD size is "an external ceiling we do not control" and that `profiles × (1 +
satellites)` `/64`s are needed — eighteen for a modest deployment; a `/60` is not enough.

#3862 reports that **`wan_prefix` never reads the delegated prefix size from netifd and defaults to a
hardcoded `/48`** (`backend/ctrl/src/lan.rs:339-355`). So today the router cannot tell the user what
their real ceiling is, and would cheerfully report headroom that doesn't exist. Satellites multiply
the demand against a number the router is currently making up. That is a prerequisite, not a
footnote, and the draft should cite it as one.

#3863 (disabling WAN IPv6 deletes the `wan6` section, leaving IPv6 diagnostics returning bare "Not
found") is adjacent: satellites are WAN-less by definition, so every code path that assumes a `wan6`
section exists is a satellite path too.

### 4.3 #3667 — the neighbor-table trust model already has a known hijack

Concern 14 argues that `arrival_matches` cannot apply to a routed satellite client and must be
_replaced_ by "arrival on satellite X's tunnel + address inside a subnet the Core allocated X."

#3667 documents that the _existing_ IPv6 published-port path trusts "whatever the neighbor table says
about this MAC," and that this is spoofable by anyone on the victim's segment — the novel harm being
persistence and WAN reach. That is directly relevant prior reasoning about exactly the trust
substitution the draft is proposing, and it supports the draft's argument: a satellite's tunnel
identity is arguably _stronger_ evidence than the neighbor table the Core trusts today. Citing it
would turn concern 14 from a worry into an argument.

### 4.4 #3672 / #3673 — profile DNS already leaks on VPN-routed profiles

Concern 9 asks whether a satellite runs its own resolver or forwards to the Core, and notes the
answer determines whether a child profile's DNS filtering is enforced identically on both routers.

Two open bugs say the single-router case is already broken in that direction: #3673 (custom DNS on a
VPN-routed profile resolves outside the tunnel — SmartDNS upstreams egress the WAN) and #3672 (DNS
leaks to the ISP when the WireGuard config carries no DNS servers). Replicating the profile's DNS
behavior onto a satellite replicates those bugs to a second box, and a WAN-less satellite has no WAN
to leak _to_, which may make the satellite path behave differently from the Core path for reasons
nobody intended. Worth naming as a dependency.

### 4.5 #3676 — Multiple WAN collides with satellite egress

#3676 proposes replacing the hardcoded `"wan"` constant with a **WAN identity** threaded through
`wan.rs`, `ethernet.rs`, profile egress, published ports and the firewall seed.

The satellite design is the same refactor viewed from the other end: it needs profile egress to stop
assuming a single local WAN (concern 4's WAN-less egress) and needs the published-port `src: "wan"`
assumption parameterized. These two features want the same abstraction and should share it — or the
second one to land will fight the first. Neither issue currently knows about the other.

### 4.6 #3466 — Wi-Fi channel selection is already an open issue

Concern 8 asks who coordinates channels across routers broadcasting one SSID. #3466 (helix-nine) is
an open request for regulatory-domain-aware channel selection, a channel-width selector and
band-aware fields. Multi-router channel planning is the natural extension of that work, and concern 8
should be phrased as "extends #3466," not as a fresh open question.

### 4.7 #3662 — backup encryption is being reworked right now

Concern 20 asks whether a Core's backup includes the satellite registry and pairings, and what
happens to satellites after a restore. #3662 is an open, 6KB plan to encrypt StartWRT backups with
the admin password StartOS-style. A satellite pairing token and its WireGuard private keys are
exactly the kind of material that decides that design. Concern 20 should be a comment on #3662 as
much as a line in this issue.

---

## 5. The prior art the draft misses entirely: StartTunnel

The draft's "Alternatives considered" surveys RADIUS, L2 extension, trunk tunnels, config
replication, runtime roles, dumb APs and Wi-Fi backhaul. It does not mention **StartTunnel** — Start9's
own VPS-hosted virtual private router — once.

From #3682's audit, StartTunnel already has, in production:

- a **subnets-as-hub model** (the draft reinvents this as "routed attachment, per-router subnets");
- **per-device policy and a device registry** keyed off tunnel peers (the draft proposes building
  this from scratch as concern 17 / D13);
- a **PCP + UPnP gateway** with PORT_SET, ANNOUNCE and the HOSTNAME option (the draft's concern 14);
- **SNI hostname routes** persisted across restart;
- **live state sync** between gateway and clients (the draft's semantic config push);
- authorization of **a WireGuard peer by tunnel address and public key** — which is, almost exactly,
  the substitute check the draft proposes in concern 14 and calls new.

And #3681 ("Refactor: share code with StartTunnel") plus #3682 ("Port missing StartTunnel
functionality to StartWRT") are open issues whose entire premise is that these two products should
converge.

A maintainer reading the draft will think of StartTunnel within the first two paragraphs of the
proposed solution, because a satellite is structurally a StartTunnel gateway with the direction of
the WAN reversed. The draft's failure to say "I looked at StartTunnel, and here is what carries over
and what doesn't" is the single thing most likely to cost it credibility with this particular
audience. It is also, probably, a real opportunity: several of the draft's "Large (new)" modules may
already exist in `shared-libs/crates/start-core/src/tunnel/`.

---

## 6. Where the draft is stronger than the corpus

**Concern 1 (the one-LAN-port problem) is a finding, not a question.** `docs/src/hardware.md`
specifies 1 × gigabit WAN and 1 × gigabit LAN. No issue in the tracker discusses port count as a
design constraint, and the satellite design's own runbook casually uses `lan2` and `lan4`. The
observation that a wired backhaul consumes the satellite's _only_ client-facing port — making a
satellite Wi-Fi-only, and making the deferred Wi-Fi backhaul potentially load-bearing — is exactly
the kind of concrete, verifiable, plan-changing finding this tracker rewards. It is the strongest
paragraph in the draft.

**The concerns list is more disciplined than the corpus's equivalent.** The corpus embeds decisions
inline, which means they are easy to lose. Collecting them, ordering them by blast radius, and
separating "wrong answer changes what gets built" from "implementation detail" is better practice
than what the tracker currently does. It is the part of the draft most worth preserving at full
length even if the rest is cut.

**Stating what has not been tested.** Very few issues have to make this disclosure; the draft does,
prominently, and proposes the cheapest possible experiment (two routers, a cable, one ping) to
resolve the top risk. That is the right instinct and it is well executed.

**Alternatives with reasons for rejection.** The template asks for this and the corpus rarely
supplies it — #3682's "Not porting" section is the closest analogue and it is a list, not an
argument. The draft's table, minus the StartTunnel omission, is better than the house standard.

---

## 7. If the goal is for this to land, not just to be filed

Ordered by expected effect. None of these are edits to the draft; they are options for what to do
with it.

1. **Split the venue.** File a ~1,500–2,500 character issue in house style — problem, the shape of
   the solution in one paragraph, the top five concerns, links to neighbours — and put the full
   design in `rfcs/` as a PR. The repo already does this: `rfcs/sni-demux-kernel-handoff.md` is a
   "draft spec for implementation" with a named owner, and #3682 defers a design decision to
   `rfcs/startwrt-dns-injection.md`. A 22KB issue has no natural reviewer; an RFC PR does.
2. **Add the "Related" line and reconcile with #3670.** #3670 is the one collision that changes the
   plan. Silence on it is the draft's biggest credibility risk.
3. **Address StartTunnel explicitly**, even in two sentences.
4. **Anchor the code claims to `file.rs:line`.** The facts are already verified; the citations cost
   nothing and convert assertions into findings in this tracker's idiom.
5. **Convert the phases to `- [ ]` checkboxes** and say "tracking issue," matching #3670.
6. **Lead with concern 1.** The hardware constraint is the most likely thing to change a
   maintainer's mind about feasibility, and it currently sits behind ~9,000 characters of framing.
7. **Know where it will route.** Type `Feature` assigns to MattDHill; the `StartWRT` label needs
   either a manual application or the template's `### Project` heading. Choosing the bug-shaped
   template would reach `dominion5254` but misrepresent the issue.

---

## 8. One-line verdict

The draft is substantively stronger than the average issue in this tracker and stylistically
unlike any of them: better at stating uncertainty, better at separating decisions from details, and
carrying at least one finding (the port count) that nobody else has written down — but three times
too long for its venue, missing the `file:line` anchoring the corpus runs on, and silent on the two
open issues (#3670, #3862) and the one sibling product (StartTunnel) that a maintainer will think of
immediately.
