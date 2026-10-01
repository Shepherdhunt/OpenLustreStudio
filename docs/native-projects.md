# Local native projects, format v1

This opt-in increment composes independent local model projects through a
`.olproj` manifest. The compiler reads each project's authored model files,
verifies every dependency identity and exact content snapshot, qualifies its
symbols, and builds transient IR. Loading and generation never rewrite the
library models or copy their declarations into the consumer's source.

## Manifest

```json
{
  "format": "openlustre.project/v1",
  "project_id": "example.consumer",
  "model": "consumer.wksc",
  "entrypoint": "Root",
  "dependencies": [
    {
      "alias": "flight",
      "project": "../library/project.olproj",
      "project_id": "example.library",
      "snapshot_sha256": "REPLACE_WITH_THE_64_HEX_DIGIT_LIBRARY_SNAPSHOT"
    }
  ],
  "exports": {
    "nodes": ["Root"],
    "types": [],
    "constants": [],
    "contracts": []
  }
}
```

All fields are required; unknown manifest fields are errors. A project ID is
1–128 printable, non-space ASCII characters and is chosen by the author.
Changing it changes nominal type and symbol identity. IDs have no registry or
cryptographic authorship guarantee. Entry points and export lists name local
declarations. An entry point may be private; it is still the project's default
root when that manifest is loaded directly. Consumer aliases are unqualified
ASCII identifiers, unique within their own manifest. An alias matching a local
enum type is rejected to avoid ambiguous two-part value references.

The `model` is a JSON, `.wksc`, or YAML IR file using the existing `Project`
schema. Same-project `includes` remain available for owned fragments. Every
owned file must be a relative regular file within the manifest's directory;
canonical paths reject outside-root files when inputs are stable. Resolution
assumes trusted local files that remain stable during loading; concurrent file
or symlink replacement is not security-grade filesystem containment. Snapshots
fingerprint the exact bytes parsed. Dependency paths are explicit,
relative `.olproj` references; `../` permits sibling independent projects.
Directory discovery, remote URLs, implicit dependencies and absolute dependency
paths are unsupported.

## Authored references and identity

Existing IR string fields carry qualified references: a call's `node` can be
`flight::Control`, a named type or record constructor can be
`signals::SensorSignal`, a value can be `payload::LIMIT`, and a contract name
can be `flight::Bounds`. An external enum value is
`payload::Status::Ready`, gated by the `Status` type export. Own enum values may
use `Status::Ready` or the existing bare `Ready` syntax. Unqualified references
resolve only within the owning project. An imported project's aliases do not
become aliases of its consumer. There is no implicit re-export.

Node/type/constant/contract declarations use stable identities consisting of
project ID, kind and local name. Enum variants additionally include their enum
type. Full, delimited UTF-8 hex encoding gives injective backend identifiers,
independent of the consumer alias. Distinct projects may reuse local names.
Repeated aliases and diamonds with the same project ID and snapshot resolve
once; two snapshots of one project ID in a graph are rejected. Shared nominal
records/enums therefore retain one identity; separately owned records retain
different identities even when their layouts match. Existing primitive alias
semantics are preserved.

Port/local names and record fields remain unchanged. Ordinary locals shadow
own global values. A local/port/ghost name that exactly matches a generated
global symbol or derived node/contract C artifact is rejected so a qualified
reference cannot capture it. Record fields matching generated constant macros
are also rejected; other fields and unrelated `olp_` names remain available. Export
lists constrain cross-project source references and public root selection;
they are not an access-control or security boundary. Internal compiler lookup
can reach the private declarations needed by an exported node.

Constants and types are ordered by their rewritten dependencies before
simulation/C emission. Unsupported constant and recursive type cycles are
rejected. Local state machines and activations lower before qualification;
their source provenance is retained. An activation's implicit `last()` default
for a dependency enum needs an explicit initializer in this increment.

## Snapshots and commands

```sh
openlustre project snapshot library/project.olproj
openlustre project resolve consumer/project.olproj
openlustre check consumer/project.olproj
openlustre simulate consumer/project.olproj --inputs inputs.csv
openlustre emit-clite consumer/project.olproj --out generated --driver
openlustre emit-lustre consumer/project.olproj --out lustre --root flight::Control
```

`snapshot` prints the fingerprint of one project's declared snapshot. It checks
identity, file presence and acyclic graph structure, but tolerates stale pins
for deliberate updates in dependency order. It does not update any file and
does not establish that exports, model references or imported operators load.
Update expected pins explicitly, then use `resolve` or a normal model command
to verify every edge, including unused and repeated edges. Missing files,
wrong identities, changed model/fragment bytes, changed manifest semantics,
conflicting snapshots and cycles are errors.

The SHA-256 encoding is the ASCII domain
`OpenLustre native project snapshot v1\0`, followed by a u64 big-endian
length-framed canonical manifest JSON, then length-framed relative path and
raw file bytes for every owned model, sorted by path. Canonical manifest JSON
uses the schema field order shown above, compact JSON and lowercase pin hex;
dependency/export array ordering is retained. Dependency identity, path and
expected snapshot are included in the manifest fingerprint. Absolute runtime
locations and filesystem permission bits are excluded. This detects exact
content changes; it is not a signed package or a version-range resolver.

`resolve` prints graph and symbol provenance. CLI generated output includes
`native_resolution.json` beside existing traceability reports; its symbol map
connects backend names to owner, local name, export status and source file.
Evidence JSON retains the complete verified graph; evidence HTML lists project
snapshot identities. Manifest paths in these reports are local inspection
locations. Re-resolve after relocation rather than treating them as portable
filesystem paths.

Node symbols also expose `artifact_basename`: `ol_artifact_` plus the full
64-hex SHA-256 digest of `OpenLustre native artifact name v1\0` followed by
the complete resolved node name. The 76-byte stem is stable and independent
of consumer aliases. All graph node stems are checked for digest collisions;
a collision is an error rather than an overwrite. C/Lustre identifiers and
semantic report identities remain unchanged. Native Makefile targets, Studio
executables, evidence files and browser downloads use this stem with bounded
role prefixes/suffixes. Evidence and Studio responses retain both the semantic
entry and artifact mapping. Legacy project artifact names are preserved.

## Compatibility and limits

Legacy `.ols`/YAML/JSON/`.wksc` files and their legacy includes retain existing
semantics. A legacy include cannot smuggle in a native manifest. A directory
containing a `.olproj` must be opened by explicitly selecting that manifest;
it is never merged as a legacy directory or converted to a new workspace.
Native projects reject `--with-stdlib` and external `--imports` merging; every
model dependency must have an explicit manifest and pin. Studio automatically
omits its embedded model library for native projects.

Studio exposes native inspection, generation views and simulation. Native
editing, import-copy, undo/redo and Save routes return an explicit read-only
error. Native Studio Build checks the manifest entrypoint without writing
derived IR into authored files. Other Studio build-root selections return an
explicit error; CLI `--root` selects an exported dependency node. A native
project-reference editor, source-name presentation throughout the UI and
manifest/pin maintenance UI are deferred.

Imported C operators are explicitly unsupported in native v1. Existing legacy
imported-C call/wrapper ABI and import-check exit-status defects remain separate.
No native manifest can silently accept those unsupported paths.

This increment provides local exact snapshots, not a package registry,
remote fetching, semantic versions, dependency ranges, multiple versions of
one identity, target ABI packaging, or reusable independently certified proof
artifacts. Proof emission uses resolved transient IR; assumptions remain the
existing contract/prover semantics. No new Kind 2 proof or aircraft assurance
claim follows from resolver/simulation tests.

Execution remains synchronous model stepping. Manifests do not add threads,
physical scheduling, rate conversion, freshness policy or a HAL. Those must
be expressed in the composition's typed ports, clocks, held snapshots and
external adapter. Full-hex identifiers compile under the tested host Clang;
embedded compiler/linker significant-character and length limits require
separate target verification. Hardware timing, WCET, deployment, SCADE import,
and actual autopilot control laws remain unverified.

## Regression scope

`tests/native_project.rs` uses independent synthetic libraries and recurrence
oracles. It covers collisions, aliases, nominal diamonds, private references,
pin/conflict/missing-file/cycle failures, immutable source bytes and modes,
legacy includes, entrypoints, declaration ordering, construct lowering and
source provenance. The nested-state check compares 400 interleaved updates
and 1,600 values with explicit resets in IR and generated host C under address
and undefined-behavior sanitizers. These fixtures are not ExistXPilot.
