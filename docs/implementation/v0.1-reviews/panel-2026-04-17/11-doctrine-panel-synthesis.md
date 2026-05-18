# Loom Doctrine Panel — Cross-Panel Synthesis

**Panel ID:** loom-doctrine-2026-04-17
**Target documents:** `docs/suite/briefing.md`, `docs/suite/loom.md`
**Synthesiser:** Panel Synthesiser Agent
**Date:** 2026-04-17
**Panel size:** 5 personas (p1 Priya, p2 Marcus, p3 Yasmin, p4 Dev, p5 Sam)

---

## 1. Executive summary

The doctrine lands. All five personas accepted the central thesis (federation,
enrichment-not-load-bearing, pairwise composability) as a sincere and precisely
expressed architectural stance. Four of five explicitly credited the author for
naming the stealth-monolith failure mode rather than hand-waving past it.

What every technical reader found, independently and from different angles, is
that the v0.1 *implementation* described in `briefing.md` embeds two structural
couplings that the `loom.md` failure test was not designed to catch:

1. A **Wardline -> Clarion -> Filigree findings triangle** that makes the
   Wardline+Filigree pair functionally dependent on Clarion in v0.1, even though
   §4's pairwise composability rule declares this prohibited.
2. A **direct Python import** (`wardline.core.registry.REGISTRY`) from Clarion's
   plugin into Wardline's runtime, which is a startup-time, code-level
   dependency categorically different from the HTTP/file couplings in the same
   data-flow table.

Both couplings technically pass the §5 failure test as worded ("does removing a
sibling change the *meaning* of the remaining product's data?") while
operationally violating the principle the test is meant to enforce. Three of
five personas surfaced this scope mismatch independently. That is structural.

Secondary finding: the doctrine is well-written but **untested enrichment**.
Clarion is the product whose absence would most severely stress the architecture
and it does not yet exist. Every claim the doctrine makes about Clarion's
behaviour is, at present, theoretical.

---

## 2. Convergent concerns (ranked high-signal to lower-signal)

### 2.1 The Wardline -> Filigree via Clarion triangle — CONFIDENCE: HIGH

Three personas (p1 Priya, p3 Yasmin, p5 Sam) independently identified that the
Wardline+Filigree pair is not a pair in v0.1 — it is a triangle with Clarion as
required middleware. A fourth (p4 Dev) flagged the same pattern obliquely via
the "bootstrapping the suite fabric" phrase.

- **Priya (p1):** "the Filigree + Wardline pair currently requires Clarion as a
  translator ... This is a real violation of §4's pairwise composability rule,
  and neither doc calls it out explicitly."
- **Yasmin (p3):** "In v0.1, Clarion's translator is the only conduit for
  Wardline findings reaching Filigree's triage store."
- **Sam (p5):** "Wardline's findings don't go directly to Filigree. They go
  through Clarion's translator first. This means Clarion is on the critical
  path for Wardline findings to reach Filigree."
- **Dev (p4):** "If those protocols, once landed, become the connective tissue
  that other products depend on, then Clarion becomes the fabric and 'no Loom
  runtime' becomes a naming convention, not an architectural reality."

This finding passes the **convergent-reasons test**. Priya arrived via §4
pairwise composability. Yasmin arrived via finding lifecycle and audit-trail
continuity. Sam arrived via positioning/claims analysis. Dev arrived via
governance framing. The conclusions converge; the reasoning does not — these
are genuinely independent signals, not one prior repeated in four vocabularies.

The docs acknowledge it obliquely (briefing: "Wardline should eventually have a
native emitter"; loom.md: silent). All four personas explicitly noted that
"eventually" is doing heavy lifting. Marcus (p2) is the one persona who did not
surface this — consistent with his declared blind spot on data-flow details.

### 2.2 The `wardline.core.registry.REGISTRY` direct-import pattern — CONFIDENCE: HIGH

Four of five personas (p1, p3, p4, p5) flagged the direct Python import. This
is the single most-cited specific text-level concern in the panel.

- **Priya (p1):** "Direct import. Not a file read. Not an HTTP call. A Python
  import. That is a code-level, startup-time dependency... This is the row that
  concerns me most in the entire document."
- **Yasmin (p3):** "Clarion's Python plugin takes a direct runtime dependency
  on Wardline's Python package. That is a tight coupling that the 'each tool is
  independently usable' principle is supposed to prevent."
- **Dev (p4):** "That's not a narrow interop contract — that's a direct import
  dependency."
- **Sam (p5):** "The briefing mentions Wardline needing to 'commit to maintain
  legacy-decorator aliases,' which is a real ask. Has Wardline agreed to that
  constraint, and what's the versioning contract?"

All four personas noted that the data-flow table presents this row in the same
visual register as file reads and HTTP POSTs, despite it being a categorically
tighter coupling. Priya's framing — "That is not an honest presentation of the
coupling surface" — is the sharpest. Marcus (p2) did not flag it (again,
consistent with his blind spot).

This is a **Tier 1 text-surface finding**. The direct-import language is
literally in the data-flow table. No interpretation required.

### 2.3 The doctrine's failure test is scoped too narrowly — CONFIDENCE: HIGH

Three personas (p1, p3, p5) independently concluded that the §5 failure test,
as worded, is too narrow to catch the couplings they observed.

- **Priya (p1):** "The doc's failure test is too narrow. It catches semantic
  drift but not initialization coupling. The direct-import row passes the
  stated test while still being a form of coupling that would break in a
  deployment where Wardline isn't on the same Python path."
- **Yasmin (p3):** "Clarion's SARIF translator is not enrichment in the §5
  sense; it is a pipeline stage. The doctrine's enrichment principle does not
  directly govern it, and that is a gap in the doctrine's coverage."
- **Sam (p5):** "I think the doctrine's failure test technically passes. But I
  also think the failure test is drawn narrowly enough to pass while leaving
  open a real architectural question."

Convergent-reasons test: Priya focused on startup/initialization coupling;
Yasmin focused on the translator being a pipeline stage, not enrichment; Sam
focused on cross-tool workflow semantics. Three different analytical axes, same
conclusion. Structural.

### 2.4 "Untested enrichment" — doctrine outpaces implementation — CONFIDENCE: HIGH

All five personas noted the gap between doctrine-as-written and
implementation-as-exists. This is universal.

- **Marcus (p2):** "'three independent tools that enrich one another' ... The
  table tells me it's two-plus-one-on-paper-plus-one-concept."
- **Priya (p1):** "Every coupling claim in the briefing is theoretical. Good
  theory, untested reality."
- **Yasmin (p3):** [from her Key Finding] "unspecified contract in the
  implementation."
- **Dev (p4):** "Applying go/no-go to a non-design is a placeholder at best. It
  signals intent without providing evidence."
- **Sam (p5):** "What happens to the doctrine when the most structurally
  central tool ... is actually running in production?"

Because all five personas surface it, the framing divergence is instructive:
Marcus reads it as honest self-disclosure (the status table redeems the
oversold lede); Dev reads it as governance theatre (a doctrine applied to a
non-design is placeholder-grade). Same fact, two postures.

### 2.5 Identity-translation centralisation in Clarion — CONFIDENCE: MEDIUM-HIGH

Three personas (p1, p3, p5) noted that Clarion's role as reconciler for all
three identity schemes makes it the de-facto translation authority for the
suite, even though §6 disclaims a "neutral Loom identity oracle." See the
unreliable-narrator check (section 4) for Sam's partial recoil from his
stronger form of this claim.

- **Priya (p1):** "Clarion a de-facto shared identity authority. Not a shared
  store — the store is local files — but a shared authority."
- **Yasmin (p3):** "Three identity schemes, one translator. Honest. Also: a
  significant single point of governance responsibility."
- **Sam (p5):** "Clarion isn't a 'neutral identity oracle' in the sense
  loom.md §6 disclaims. But it is *the place where all identity reconciliation
  happens*."

The reasoning across all three is closely aligned, so I downgrade from HIGH to
MEDIUM-HIGH — there is a single shared prior ("centralised translation becomes
centralised infrastructure") in three vocabularies. Still substantial signal.

### 2.6 EntityId stability and schema versioning — CONFIDENCE: MEDIUM

Two personas (p3 Yasmin, p5 Sam) raised entity-ID versioning concerns. Yasmin
framed it as SOC 2 audit continuity; Sam framed it as cross-reference
resolution. Both asked: what happens to existing Filigree records when Clarion
re-keys its catalog?

- **Yasmin Q5:** "When Clarion's EntityId minting algorithm changes between
  versions, what is the migration story for Filigree records that carry stale
  EntityId cross-references?"
- **Sam Q2:** "Filigree issues reference entities by Clarion EntityId. If
  Clarion is removed from a deployment, those references become opaque
  strings."

Not universal (p1, p2, p4 did not raise it), so MEDIUM. But Yasmin's original
spec-predicted concern — she is the technical validator — lands almost
verbatim on this question. That makes it a high-quality MEDIUM finding.

### 2.7 `registry_backend` dependency inversion — CONFIDENCE: MEDIUM

One persona (Dev, p4) flagged this with full clarity; Priya (p1) noted it
partially. The briefing asks Filigree to add a pluggable `registry_backend` "so
Clarion can own the file registry." Dev noted this reverses the stated
direction of enrichment:

- **Dev (p4):** "What happens to Filigree in `--no-clarion`? If Filigree's
  registry backend silently breaks or degrades to an incoherent state without
  Clarion, that's a load-bearing dependency in the direction loom.md most
  explicitly prohibits."

Only one persona raised it to full prominence, but it is a concrete, testable,
Tier 1 text-surface concern. MEDIUM confidence purely on persona count.

### 2.8 MCP failure behaviour during consult — CONFIDENCE: LOW-MEDIUM

Priya (p1, Q5) flagged that Clarion's observation-emission path to Filigree
via MCP has no documented failure behaviour. No other persona raised it. Low
persona count but highly specific — keep as a LOW-MEDIUM noted gap.

---

## 3. Divergent reactions

### 3.1 Marcus's cautious-interest vs Dev's conditional-hostility

Reading the same text, Marcus (p2) and Dev (p4) produced opposite
dispositional verdicts:

- **Marcus (p2):** "The doctrine is not aspirational — it is defensive...
  Projects that write their doctrine as a series of named failure modes they
  have decided to prohibit tend to hold the line longer."
- **Dev (p4):** "The §5 defense of Shuttle's solo-mode validity is premature
  reassurance dressed as analysis."

Both read §3, §4, §5, §6 carefully. Both found the anti-patterns precisely
named. Marcus concluded this was evidence of battle-tested design judgement.
Dev concluded that naming an anti-pattern does not prevent it and the document
substitutes doctrine for working code.

This is the **adoption-vs-contribution tension** that the collision test (see
section 5) was designed to surface. Marcus is evaluating whether to put
Filigree+Wardline in front of his platform lead (yes, conditionally).  Dev is
evaluating whether to invest time contributing to Clarion (not yet, because
§7's go/no-go test is being applied to a non-design).

**The documents are doing their job for the adopter persona and not yet doing
their job for the contributor persona.**

### 3.2 Yasmin's compliance lens vs Sam's adversarial lens on identity

On the same identity-scheme text (briefing's "three concurrent identity
schemes" paragraph and loom.md §6's "product that cares does the translation"),
Yasmin and Sam diverged sharply:

- **Yasmin (p3):** The identity scheme passes the principle *at rest* — once
  a finding is in Filigree, Filigree's triage authority is durable. Her
  concern is *over time*: what happens when Clarion's translator changes?
- **Sam (p5):** The identity scheme passes the doctrine's failure test *as
  worded* but the doctrine's test is scoped narrowly. His concern is *scope
  of the test itself*.

The reactions are not in conflict. They are complementary. Yasmin sees a
temporal gap (version-to-version continuity); Sam sees a conceptual gap (the
test's scope). The documents answer neither question.

### 3.3 Shuttle as product vs Shuttle as feature

- **Priya (p1):** Skipped Shuttle ("proposed, no design. Not evaluating").
- **Marcus (p2):** Skipped Shuttle.
- **Yasmin (p3):** Did not mention Shuttle.
- **Dev (p4):** Read Shuttle carefully, concluded it fails criterion 2 of §7
  ("Shuttle in solo mode is a deployment script that has read its own
  architecture document").
- **Sam (p5):** Noted Shuttle approvingly ("I respect that they've thought
  about what it *isn't*").

Dev was the only persona who stress-tested Shuttle against §7. His verdict was
the most negative of anything in the panel: "The go/no-go test is being applied
to a design that doesn't exist yet... Applying go/no-go to a non-design is a
placeholder at best."

This divergence maps directly to declared reading behaviour — Dev alone was
configured to test §7 against Shuttle. The panel therefore cannot confirm or
reject Dev's conclusion; he is the only data point.

---

## 4. Unreliable-narrator check (Sam / p5)

**Pre-registered misconception:** Sam will read the Clarion identity
translation layer as evidence that Clarion is a "secret monolith" by the
doctrine's own failure test.

**Outcome: REFINED, not CONFIRMED.**

Sam triggered on the misconception in his initial reading of the data-flow
table ("the pattern I was looking for"), then pulled back after re-reading §5
and §6. His own words in the Unreliable-Narrator Note at the end of his
journal:

> "The strongest version of my critique — that Clarion is a 'secret monolith'
> by the doctrine's own failure test — doesn't actually survive careful
> reading. The failure test is about whether removing a sibling changes the
> *meaning* of another product's *own* data. Filigree's issues are meaningful
> without Clarion. Wardline's scanner output is meaningful without Clarion.
> The cross-references become unresolvable, but they don't become
> *incoherent*. The doctrine draws this line deliberately, and I think the
> line is defensible, even if I'd draw it in a slightly different place."

He refined the criticism to: "Clarion is structurally central to any workflow
that crosses tool boundaries, even though the doctrine correctly notes that
each product's *own* data remains coherent in isolation."

**What this tells you about the doc's clarity:** §5 and §6, read carefully,
*do* foreclose the "secret monolith" reading. The doctrine did its job with
Sam. But — and this is important — the reading does require careful,
slow attention to exactly what the failure test says. A skim reader would not
do this work. Sam did it because he is a hostile reader hunting for
weakness; a friendly reader under time pressure might not.

The fact that Priya (also adversarial) reached a milder version of the same
concern and did *not* recoil from it (she concluded the failure test is too
narrow, full stop) suggests the doctrine survives hostile reading but not
cleanly. The "secret monolith" misconception is landable but recoverable. The
"failure test is too narrow" refinement is landable and sticky.

---

## 5. Control-persona calibration (Marcus / p2)

Three pre-registered predictions. Auditing against Marcus's actual journal:

| Prediction | Actual | Match? |
|---|---|---|
| **Reading path:** Status table first in briefing, then loom.md §3–§5, skip §7/§8 | "First move: scan the page for a status table. Took about thirty seconds." Then loom.md §3, §4, §5, §6, skipped §7 and §8 explicitly ("Skipped, as intended"). | **YES, exact.** He read §6 which was not predicted but is adjacent to §5. Path otherwise pixel-perfect. |
| **Key concern:** Gap between three-product framing and two-tools-actually-running reality | "The lede — the one-paragraph version at the top — reads 'three independent tools that enrich one another.' That's a three-product claim. The table tells me it's two-plus-one-on-paper-plus-one-concept." | **YES, exact.** Language is almost verbatim the predicted reaction. |
| **Verdict:** Cautiously interested, pass to senior engineer, will not dismiss, local-first positioning lands | Verdict file: "Cautiously interested. Passing to a senior engineer for deeper review of Clarion's detailed design." Notes local-first positioning approvingly ("tells me there's no surprise cloud dependency to audit"). | **YES, exact.** |

All three predictions matched. The control persona is calibrated. This gives
us confidence in two things:
1. The doctrine documents are legible to the target decision-maker persona as
   specified.
2. The panel's reader-simulation machinery is producing reactions consistent
   with persona specs, not generic LLM reactions.

One unpredicted addition: Marcus surfaced the "defensive doctrine vs
aspirational doctrine" reading as his Key Finding. That was not pre-registered
and is a genuine new signal from the panel.

---

## 6. Collision-test outcomes

### 6.1 [p1 Priya, p5 Sam] — Adversarial convergence

**Prediction:** Both adversarial readers from different angles may arrive at
the same criticism ("the translation layer is load-bearing") from different
directions. If they do, it is structural.

**Outcome: CONVERGED, structurally.**

Priya arrived at "direct Python import is startup-time coupling that the
failure test misses." Sam arrived at "Clarion-as-translation-layer is
structurally central to cross-tool workflows." Different surface claims;
identical underlying structural concern. Both converged on "the §5 failure
test is scoped too narrowly to catch what v0.1 actually does."

Finding 2.3 (the failure test's scope) is the structural claim that falls out
of this collision.

### 6.2 [p2 Marcus, p4 Dev] — Adoption-vs-contribution tension

**Prediction:** Marcus evaluates for adoption, Dev for contribution.
Intersection at §7 go/no-go and composition law will reveal whether the
doctrine serves both audiences.

**Outcome: TENSION MATERIALISED. Doctrine serves adopter, not contributor.**

Marcus never engaged §7 (as predicted, blind-spot confirmed). Dev engaged §7
extensively and concluded it is premature-applied governance. Marcus's verdict
is "Cautiously interested, not ready, worth monitoring." Dev's implied verdict
is "I cannot evaluate contribution feasibility because the design whose
doctrine I am reviewing does not yet exist in code." Same docs, opposite
actionability.

**This is an editorial gap:** the docs have a clear path for a reader who
wants to adopt (status table -> principles -> pass to engineer). They do
not have a clear path for a reader who wants to contribute to the unbuilt
product. Section 13 flags this as a gap the panel revealed.

### 6.3 [p3 Yasmin, p1 Priya] — Specific-vs-general concern overlap

**Prediction:** If Priya's general "enrichment not load-bearing" concern
turns out to be Yasmin's specific finding-lifecycle concern stated
differently, the docs have a coherent blind spot.

**Outcome: CONVERGED at a specific blind spot.**

Priya (general): "the enrichment-not-load-bearing principle is enforced by
human review at design time, which is the weakest possible enforcement."
Yasmin (specific): "an unspecified deduplication key is a finding in its own
right, because audit trail continuity depends on it."

Both concerns reduce to: **there is no documented mechanism, test, or
contract that enforces the principle at the implementation layer.** Priya
asks about CI isolation tests. Yasmin asks about cross-version finding
identity. The doctrine's blind spot is the same: stated principle, unstated
enforcement mechanism.

This is the single highest-value collision-test outcome. Finding 2.3 and
this collision are two framings of the same doctrine gap.

---

## 7. Epistemic confidence grading summary

| # | Finding | Personas | Tier | Confidence |
|---|---|---|---|---|
| 2.1 | Wardline+Filigree-via-Clarion triangle violates §4 | p1, p3, p5 (+p4 oblique) | Tier 1 text-surface | HIGH |
| 2.2 | `wardline.core.registry.REGISTRY` direct import | p1, p3, p4, p5 | Tier 1 text-surface | HIGH |
| 2.3 | §5 failure test scoped too narrowly | p1, p3, p5 | Tier 2 affective/analytical | HIGH |
| 2.4 | Doctrine outpaces implementation (untested enrichment) | all 5 | Tier 1 + Tier 3 | HIGH |
| 2.5 | Identity reconciliation centralised in Clarion | p1, p3, p5 | Tier 2 | MEDIUM-HIGH |
| 2.6 | EntityId stability / schema versioning | p3, p5 | Tier 3 institutional | MEDIUM |
| 2.7 | `registry_backend` dependency inversion | p4 (+p1 partial) | Tier 1 text-surface | MEDIUM |
| 2.8 | MCP failure behaviour undocumented | p1 | Tier 1 text-surface | LOW-MEDIUM |
| 6.2 | Doctrine serves adopter, not contributor | p2 vs p4 collision | Tier 3 | HIGH (as gap) |

Tier 1 (text-surface, most defensible): findings 2.1, 2.2, 2.4, 2.7, 2.8.
Tier 2 (affective/analytical, interpretive): 2.3, 2.5.
Tier 3 (institutional, author/implementer judgement): 2.6, 2.4 (partial), 6.2.

---

## 8. Panel gaps revealed

Two personas flagged as deferred in the config (new-grad onboarder;
Filigree/Wardline maintainer reading the sibling-tool spec) were correctly
deferred — neither would have changed the synthesis. No panel member filled
these roles, and the doctrine documents are not the right surface for either.

One gap the panel *did* reveal, not pre-registered:

- **Contributor onboarding persona (not Dev).** Dev is a maintainer of a
  competing OSS tool evaluating whether to contribute. What the panel lacks
  is someone evaluating "I have been asked to add a Python plugin to Clarion
  and I do not yet have opinions about Loom as an ecosystem." Dev's Q5 and
  Q3 point at this gap directly: "What does 'designed, not yet built' mean
  for contribution entry points?" A less-adversarial contributor-oriented
  reader would test whether `loom.md` is a useful read for someone who has
  already decided to contribute, or whether it is primarily a doctrine for
  the project author.

The config's "elspeth team member" explicit gap remains unfilled. The docs
repeatedly reference elspeth as Clarion's first customer. No panel persona
represents that viewpoint. Findings 2.4 and 2.6 would benefit most from that
reader's ground-truth perspective.

---

## 9. Verdict for the author

The doctrine documents are doing their job. For a decision-maker (Marcus) they
pass the 10-minute filter and earn a "cautiously interested, pass to senior
engineer" verdict on the first read — this is exactly what a positioning
document of this kind should achieve. For an adversarial domain expert (Sam)
the documents survive hostile reading: his strongest misread (the "secret
monolith" charge) recovers on careful re-reading of §5 and §6, and the refined
version of his critique is fair and landable. For the compliance reader
(Yasmin) the doctrine provides the right conceptual frame ("findings are
facts") even though it does not answer her implementation questions — which is
correct scope for a doctrine document.

What the documents do *not* yet do is hold up against their own data-flow
table. Three of five readers, arriving from different angles, concluded that
the §5 failure test is scoped narrowly enough to pass the two couplings that
are structurally tightest in v0.1: the `wardline.core.registry.REGISTRY`
direct Python import, and the Wardline -> Clarion -> Filigree SARIF triangle.
Your doctrine is honest about the former (the briefing's "what the suite
needs" section names the REGISTRY_VERSION ask explicitly) and evasive about
the latter ("eventually" is doing heavy lifting in the claim that
Wardline+Filigree is a pairwise-composable pair). The single highest-leverage
edit is this: **add a concrete paragraph to §5 that tests the failure principle
against the current data-flow table and names the v0.1 asterisks explicitly.**
Call out the SARIF triangle as a known temporary violation of the pairwise
rule with a retirement condition (native Wardline emitter). Call out the
Python import as initialization-time coupling that the failure test as
currently worded does not catch, and either broaden the failure test to
include initialization coupling or accept the scoping gap and document it.
This edit costs you perhaps 200 words. It converts Priya, Yasmin, and Sam
from "the doctrine is good but blind to v0.1 reality" to "the doctrine names
its own v0.1 asterisks and commits to resolving them." That is a very
different posture for a pre-public positioning document. The doctrine is
strong enough to survive being this honest about its current scope.

---

## 10. Recommended actions (ranked by leverage)

1. **Broaden §5's failure test** or explicitly scope it. The current wording
   ("changes the *meaning* of another product's own data") catches semantic
   drift but not initialization coupling, pipeline-stage dependencies, or
   cross-tool workflow availability. Either accept the scope and document what
   is out of scope, or expand the test. Either is better than the current
   implicit gap. [Addresses findings 2.1, 2.2, 2.3]

2. **Rewrite the `briefing.md` lede** from "three independent tools" to "two
   tools in production, one in design, one proposed." Marcus asked for exactly
   this edit. Minor. Cheap. High trust payoff. [Addresses 2.4]

3. **Add a "pairwise composability in v0.1" subsection** to `briefing.md` or
   `loom.md` that enumerates each pair and tests it against §4 honestly.
   Wardline+Filigree today is a triangle; say so. Wardline+Clarion is a pair;
   say so. Clarion+Filigree is a pair; say so. This defuses the convergent
   concern. [Addresses 2.1]

4. **Document the `wardline.core.registry.REGISTRY` import contract**.
   Versioning policy, failure mode when Wardline is absent from the Python
   path, and what "legacy aliases" means as a commitment (best-effort vs
   release-train gate). Four of five personas asked variants of this
   question. [Addresses 2.2]

5. **Add a contributor-facing "how Loom serves contributors" pointer** or a
   `CONTRIBUTING.md`. Dev's journal reveals that `loom.md` reads as
   governance self-binding when viewed from the contributor seat. A one-line
   pointer to contributor docs — even if those docs are "not yet written, see
   `v0.1/README.md`" — repositions the doctrine correctly. [Addresses 6.2]

6. **Specify the finding-deduplication key contract** across Clarion
   translator versions. Yasmin's Q1 is the most specific unanswered question
   in the panel and lives below doctrine level — but a single sentence in the
   doctrine ("stable deduplication identity across translator versions is a
   Clarion release-train gate") would neutralise the concern. [Addresses 2.6]

---

## 11. What the panel did not find

Worth recording what *did not* surface, because absences matter:

- **No persona challenged the four-product structure** (Clarion, Filigree,
  Wardline, Shuttle). The bounded-domain table in §2 was uniformly praised.
- **No persona found the "loom" metaphor unhelpful**, though Priya flagged it
  as discount-worthy marketing. She did not retract that view, but it did not
  harden into a finding.
- **No persona rejected "local-first, single-binary, git-committable state"**
  — Marcus particularly praised it; Yasmin flagged one edge case (degraded
  JSONL outside audit scope) but did not reject the principle.
- **No persona accused the doctrine of bad faith.** Adversarial readers
  (Priya, Sam, Dev) each explicitly noted that the authors had named the
  failure mode they were defending against, and credited this. The critique
  is "scope is too narrow," not "the scope is dishonest."

These absences are as important as the findings. They tell you what the
documents are doing well and should not be edited away.

---

## 12. Method note

Findings in sections 2, 3, and 6 derive from close reading of five persona
journals totaling approximately 18 pages of primary material. Every quoted
sentence is verbatim from the persona journal. Counts are counted, not
summarised. The convergent-reasons test was applied to each HIGH-confidence
finding: findings where multiple personas reached the same conclusion via the
same reasoning are downgraded (see finding 2.5). Findings where multiple
personas reached the same conclusion via genuinely different reasoning are
promoted (2.1, 2.2, 2.3).

The control-persona calibration (section 5) passed on all three
pre-registered predictions. This supports the reliability of the other
persona outputs — the simulation machinery is tracking the specified personas
rather than producing generic reactions.

---

## 13. Appendix: persona priority and finding attribution

| Finding | p1 Priya | p2 Marcus | p3 Yasmin | p4 Dev | p5 Sam |
|---|---|---|---|---|---|
| 2.1 Triangle | primary | — | primary | oblique | primary |
| 2.2 Direct import | primary | — | primary | primary | primary |
| 2.3 Test too narrow | primary | — | primary | — | primary |
| 2.4 Untested enrichment | primary | primary | primary | primary | primary |
| 2.5 Identity central | primary | — | primary | — | primary |
| 2.6 EntityId stability | — | — | primary | — | primary |
| 2.7 registry_backend | partial | — | — | primary | — |
| 2.8 MCP failure mode | primary | — | — | — | — |

Marcus's absence from technical findings is consistent with his declared
blind spot ("Not tracking the technical detail of data flows"). He produced
the strongest dispositional finding (2.4 framed as "defensive not
aspirational doctrine") and the control calibration for the panel as a whole.

End of synthesis.
