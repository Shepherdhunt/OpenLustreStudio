# Model files from each release

One folder per release, holding sample model files exactly as that release
wrote them, with their scenarios. `tests/model_format.rs` checks that every
release's files still load and that their scenarios still give the
recorded results. Never edit these files; add a folder when the format
changes (see `docs/model-format.md`).

- `0.1.0/` — model format 1, written before files carried
  `format_version`: the PMS workspace (`pms.wksc` including `types.json`)
  and the release-logic model (as JSON, and the same model as YAML).
