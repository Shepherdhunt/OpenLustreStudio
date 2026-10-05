# Model file format and its version

A model is a set of files with one schema (the `Project` of `ol_ir`):

| file | syntax | what |
|---|---|---|
| `<name>.wksc` (or a legacy `project.json`) | JSON | the workspace: operators, contracts, state machines, activations, diagram layout |
| `types.json` | JSON | named types, included by the workspace (`includes`) |
| any other included file, `*.json` | JSON | more of the same, merged by package |
| `*.ols`, `*.yaml`, `*.yml` | YAML | the same schema in YAML |

Scenarios (`scenarios/*.csv`), generated `.lus` files and build output are
not model files.

## The version

Every model file starts with the format it is written in:

```json
{
  "format_version": 1,
  "name": "PMS",
  ...
```

- **Older files are upgraded.** A release reads every earlier format: the
  file is upgraded in memory, one migration per format step, before it is
  used. Files with no `format_version` — everything written by OpenLustre
  Studio 0.1.0 and earlier — are format 1.
- **Newer files are refused.** A file from a newer release is not opened:
  *"written by a newer OpenLustre Studio (model format 2); this one reads
  model formats up to 1 — update OpenLustre Studio to open it"*. Reading it
  anyway could misread it, and the next save would silently drop what this
  release does not know.
- **Files are written in the current format.** Every save writes
  `format_version` first. The save that upgrades a file from an older
  format first keeps the file as it was, as `<file>.format<N>.bak` next to
  it (once), so an upgrade can always be undone by hand.

| release | model format |
|---|---|
| 0.1.0 | 1 (files carry no `format_version`) |
| after 0.1.0 | 1 (files carry `"format_version": 1`) |

## Changing the format

A change that an older release would misread, or would drop when it saves
the file — a new field, a renamed or restructured one, a new meaning for
an existing one — needs a new format, so that older releases refuse the
file instead. Only data that is harmless to lose (a cache, a view
preference) may be added without one.

1. Add a fixture: copy the sample files the current release writes into
   `tests/fixtures/format/<release>/` (the `pms` and `release_logic`
   samples, with their scenarios). Fixtures are never edited afterwards.
2. In `crates/ol_ir/src/format.rs`, bump `FORMAT_VERSION` and add a
   `Migration { from: <old>, summary, apply }` to `MIGRATIONS`. `apply`
   rewrites the file, held as JSON, from the old format to the new one.
3. Change the `Project` types.
4. Add the release to the table above.

`tests/model_format.rs` then checks that the files of every release in
`tests/fixtures/format/` still load and that their scenarios still give
their recorded results, and the unit tests in `format.rs` cover the
upgrade chain, refusal of newer formats and the backup.
