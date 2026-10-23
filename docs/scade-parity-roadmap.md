# OpenLustre Studio — roadmap

Status as of 2026-10-05 (main at 83c88b9, plus the model format version).
This document is the plan: where the Studio stands, what a gap review and a
scale test found, and what to build next, in priority order.
`docs/SCADE_GAP_ANALYSIS.md` is the earlier, June analysis, kept for its
history log.

## Positioning (decided)

OpenLustre Studio is a free tool for **demonstration and prototyping**, for
the foreseeable future not a certified one: a way to work in the SCADE
style — graphical model → simulation → generated C → proof → evidence —
while waiting for Ansys SCADE, with the ability to carry the work into SCADE
later (item 15). It ships for Windows, Ubuntu Linux and macOS (item 16).
Qualification (DO-178C / DO-330) is out of scope.

The project's own goal stays the centre: *the model is equations +
contracts + modes + evidence*, with first-class contracts, Kind 2 proof, and
generated C that provably behaves like the simulated model.

## Where things stand

| Capability | CLI / engine | Studio |
|---|---|---|
| Dataflow authoring | ✅ | ✅ SCADE-style glyphs, orthogonal wires, zoom/pan, minimap, align/distribute, clipboard, undo/redo |
| State machines (flat, hierarchical, operator-owned) | ✅ | ✅ textual editor, draggable chart, canvas block |
| Conditional activation (activate-if), `last(v)` | ✅ clocked, frozen when inactive | ✅ editor, decision-tree chart |
| Clocks (`when` / `merge`), arrays, `map` / `fold` | ✅ boolean clocks; stateless single-output iterators | ✅ |
| Type and clock checking | ✅ | ✅ errors on boxes and wires |
| Contracts: assume / guarantee / modes | ✅ checker, CoCoSpec, runtime monitors in sim and C | ✅ contract editor, mode table, live checking |
| Import Lustre | ✅ | ✅ (errors carry no line/column yet) |
| C generation | ✅ `@trace` per equation, trace matrix, generation report | ✅ C ↔ model navigation |
| Compile & run | ✅ host compiler | ✅ host only (cross-compilation: item 7) |
| Simulation | ✅ incremental, breakpoints, contract stops | ✅ live values, waveform, C in the loop |
| Tests, coverage | ✅ golden traces on model **and** compiled C; decision coverage, unique-cause MC/DC | ✅ Tests dock |
| Kind 2 proof | ✅ bmc-ind, realizability, mode coverage, runtime errors over machine integers | ✅ Verify dock, counterexample waveforms; bundled on Linux/macOS |
| Evidence report | ✅ HTML + JSON | ✅ Project ▸ Evidence Report |
| Imported C operators | ✅ manifests, wrappers | ❌ cannot be registered or placed (item 14) |
| Downloads | ✅ Windows Setup.exe + zip, Ubuntu .deb + tarball, macOS .pkg + tarball (Apple Silicon, Intel), smoke-tested in CI | — |
| Studio access control | ✅ per-launch token, Host and Origin checks (item 17) | ✅ |
| Model file format | ✅ versioned; older formats upgraded, newer refused (item 19) | ✅ backup before an upgrading save |
| Export to SCADE | ❌ (item 15) | ❌ |

### Scale (measured 2026-10-05)

A generated model of 200 operators and about 5,000 equations (25× the PMS
sample), on one Linux machine:

| Step | Time |
|---|---|
| Import Lustre | 0.17 s |
| Check | 0.07 s |
| Generate C (19,000 lines) | 0.09 s |
| Studio requests (tree, diagram, Lustre view) | ≤ 0.15 s |
| Compile the C (`-O2`) | 6 s |
| Simulate 2,000 cycles | 9.6 s (≈ 200 cycles/s; ≈ 1 µs per equation per cycle) |
| Kind 2 (4,800 runtime-error checks, one run) | **no result in 10 min** |

Editing, checking and code generation scale; the simulator is usable but
slow for long runs (item 24); proof does not scale past PMS-sized models
(item 22).

## Done

| # | Item | Notes |
|---|---|---|
| 1 | Contract editor + mode table | Building it found untyped contract clauses (now C0080), wrong library contracts, and stateless monitors (now observer nodes in sim and C). |
| 2 | Stateful simulation session, live values on the diagram | Step / Run N / breakpoints / stop on violation. |
| 3 | Waveform viewer | Live lanes, golden vs run, counterexamples; replay in the simulator. |
| 4 | Clocked activation, `last(v)` | SCADE semantics: branches freeze when inactive. |
| 5 | Model-to-code traceability | `@trace` per equation, trace matrix, generation report with SHA-256. |
| 6, 6b | C in the loop; `float32` fidelity | Found that the simulator did not wrap sized integers and computed `float32` in double. |
| 12 | Kind 2 provisioning | `kind2 doctor` / `install`; a dedicated Kind 2 view whose meaning matches the simulator and the C. |
| 12b | Runtime errors over machine integers | Overflow, narrowing, division by zero, bounds, conversions — proved in the root's context. |
| 13 | Evidence report | Per operator: checks, contract, proof, tests and coverage, model ≡ C, traceability. |
| 16 | Downloads for Windows, Ubuntu and macOS | Found FMA contraction on ARM, Windows line endings, and Kind 2's Intel build needing Homebrew's ZeroMQ (now bundled). |
| 17 | Studio access control | See below. |
| 19 | Model file format version | Every model file starts with `format_version` (now 1; files without one — 0.1.0 — are format 1). Older formats are upgraded in memory one migration at a time, newer ones refused with a clear message instead of misread; the save that upgrades a file keeps the old one as `<file>.format<N>.bak`. The 0.1.0 sample files are kept as fixtures whose scenarios must still pass (`docs/model-format.md`). |

**17 — Studio access control (done 2026-10-05).** The Studio listened on
127.0.0.1 only, but answered any request: a web page open in the same
browser could edit the model, create folders, write generated files into
any folder and start compiles (cross-site requests), and a page whose
domain resolved to 127.0.0.1 could read the model (DNS rebinding). Now
every request must address a loopback name (`Host`), come from the
Studio's own page when a browser sends it (`Origin`), and carry the token
drawn at each launch — a `SameSite=Strict`, `HttpOnly` cookie set by the
launch link, or an `X-OpenLustre-Token` header for scripts
(`OPENLUSTRE_STUDIO_TOKEN` fixes it). Responses forbid framing, sniffing,
caching, cross-origin reads and `Referer`. Checked by unit tests, an
end-to-end socket test, the installers' smoke tests on all three OSes, and
in Chromium against an attacking page on another local port.

## Next, in priority order

Sizes: **S** ≈ a session, **M** ≈ 2–3 sessions, **L** ≈ 4+.

### P0 — before customers use it

18. **User documentation (M).** Today: the README and planning documents.
    Needed: a getting-started tutorial (build an operator, simulate, test,
    generate C, prove — 30 minutes), a PMS walkthrough, a reference for each
    dock, the supported language with every error code (E0xxx, C0xxx), a
    "coming from SCADE" page (pairs with item 15a), troubleshooting.
20. **Automated Studio UI tests (M).** The Studio is a 7,500-line page with
    no browser tests in CI; every UI check so far was by hand. A Playwright
    suite (open the PMS, edit, simulate, test, generate, prove, evidence)
    on all three OSes, using the Chromium the runners provide.
21. **Release basics (S each).** A tagged release (`v0.1.0`; tags are
    pushed from a maintainer's machine or GitHub's Releases page), a
    CHANGELOG, SECURITY.md (how to report a vulnerability), issue
    templates, `--version` naming the build (it prints `0.1.0` for every
    dev build), the Windows installer's icon and a `.wksc` file association.
    Code signing (Windows SmartScreen, macOS notarization) is a certificate
    cost, not code.
31. **Studio UI hardening (M).** Models shared by others are untrusted
    input: audit every place model text reaches the page (`innerHTML`) for
    script injection, then move scripts out of inline handlers so a
    Content-Security-Policy can forbid inline script. Item 17 keeps other
    pages out; this keeps a malicious model from acting inside the page.

### P1 — the bridge to SCADE (item 15), timed to the SCADE licence

15. **Export to SCADE (L).** A SCADE Suite licence is expected within six
    months; the work splits so that everything not needing SCADE is ready
    when it arrives.
    - **15a — now, without SCADE:** a construct-by-construct mapping to
      Scade 6 (types and constants; operators; `pre` / `->` / `fby`;
      `last`; activate-if → `activate … if`; state machines → `automaton`,
      strong and weak transitions; contracts → observer operators or
      annotations; imported C → imported operators), written from the
      published Scade 6 language reference; a **compatibility check** in
      the Studio that flags anything with no SCADE equivalent, so
      customers don't build into a dead end; a textual `.scade` exporter
      for the dataflow subset first, then state machines and activations,
      with golden-file tests on the PMS and release_logic samples.
    - **15b — when the licence arrives:** run SCADE's checker and KCG on
      every sample's export and fix what they reject; compare KCG's
      generated C against ours on the samples' scenarios (the same
      trace-equivalence test as model ≡ C); add the project file and the
      diagram layout where SCADE's graphical format allows.
    - **15c — later, if wanted:** import from SCADE (models that start in
      SCADE and come here for demos).

### P1 — proof and simulation at scale

22. **Modular proofs (M–L).** Today one Kind 2 run checks the whole model,
    with runtime-error checks lifted to the root per call; Kind 2's modular
    and compositional modes are not used. Prove each operator on its own,
    callees abstracted by their contracts; check runtime errors per
    operator under its assumptions; run operators in parallel; cache
    results by the per-operator fingerprint the evidence report already
    computes, so an edit re-proves only what changed; report partial
    results on timeout.
23. **`prove` on a timeout with no results (S, bug).** When Kind 2 times out
    before reporting anything, `openlustre prove` exits with "Kind 2
    reported no properties"; it should list every check as unknown (timed
    out), as it does when some results arrive.
24. **Simulator speed (M).** The simulator interprets the model (≈ 1 µs per
    equation per cycle). Run long batch tests on the compiled C (already
    wired for C in the loop), or compile the model to a faster form.
25. **Kind 2 on Windows (S–M).** No native Kind 2 build exists; a guided WSL
    setup from the Verify dock (detect WSL, install Kind 2 + Z3 inside it,
    point `OPENLUSTRE_KIND2` at the wrapper).

### P2 — targets and code

7. **Target integration (M).** Cross-compilation toolchains (the Compile
   dialog's disabled option), cyclic-task templates (bare-metal loop, RTOS
   task), configurable symbol prefixes. A demo on a real board (STM32,
   Raspberry Pi) shows the whole chain.
27. **Generated-code checks (M).** MISRA C on the generated code (cppcheck's
    MISRA add-on) in CI with a deviation list; stack-usage and code-size
    reports in the generation report.
14. **Imported C operators in the Studio (M).** Register a manifest, place
    it as a block, see its contract.

### P2 — language and diagram depth (by customer demand)

26. **Scade 6 language coverage (L in total).** Iterators beyond `map` /
    `fold` over stateless single-output functions (`mapi`, `foldi`,
    `mapfold`, partial iterators `mapw` / `foldw`, stateful iteration);
    generic operators (type and size parameters); enumerated clocks;
    state-machine signals and richer parallel regions; line and column in
    Import Lustre errors. Item 15a's mapping shows which matter first for
    export.
8. **Auto-layout (M).** A layered arrangement for imported and large
   operators — today a 200-call operator imports as one long column.
9. **Drawing states, transitions and branches on the charts (L).** Keep the
   textual editors as the power path.
10. **Multiple diagrams per operator, annotations (S–M).**
11. **Contract and observer blocks on the canvas (M).**

### P3 — verification depth

28. **Test generation (M).** Ask Kind 2 for scenarios that cover the MC/DC
    conditions the tests miss; masking MC/DC for coupled conditions.
29. **Requirements traceability (M).** Requirement objects (today:
    contract clause names), ReqIF import / export, coverage per
    requirement in the evidence report.
30. **Later.** Semantic model diff and merge for git; a design-document
    generator; FMU export for co-simulation.

## Suggested order

1. ✅ 17 Studio access control.
2. ✅ 19 model format version; 23 the timeout bug — small, and it keeps
   long proofs from ending in an error.
3. 18 documentation and 20 UI tests — before wider demos.
4. 15a SCADE mapping, compatibility check and textual export — ready
   when the licence arrives (then 15b).
5. 22 modular proofs — models bigger than the PMS.
6. 31 UI hardening, 21 release basics.
7. 7 target integration (a board demo), then the rest by what customers
   ask for.

### Deliberately not now

- **Rewriting the GUI on Tauri + ReactFlow** (the plan's long-term stack).
  The in-binary page works, is tested end to end through its API, and
  ships on three OSes; item 20 gives it browser tests instead.

## Reference project

[`examples/pms`](../examples/pms) (its own repository,
`Shepherdhunt/PayloadManagementSystem`) — a drone Payload Management System
built end to end in the Studio: Import Lustre, a state machine, an
activation decision tree, contracts on every operator, scenarios with full
MC/DC, model ≡ C, 171 Kind 2 proofs (60 of them runtime-error checks) and
a PASS evidence report. Growing it into a much larger sample (more
stations and airframes, sensor redundancy, release safety, mode logic,
multi-rate timing, flight-stack integration) is proposed; its size will
also exercise items 22 and 24.
