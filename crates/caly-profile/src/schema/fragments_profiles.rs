//! Generated `config.d/98-profiles.yaml` fragment text.
//!
//! The default ships with an empty `profiles: []` so a fresh
//! install has zero declared profiles. The operator uncomments
//! the example entries to opt into either a remote, local, or
//! merge profile. The `CALY_PROFILE` environment variable
//! continues to select the *active* profile from the older
//! `profiles/<name>.yaml` lookup; the new `profiles:` block is
//! the *resource declaration* used by `caly profile add|list|
//! refresh|remove`.
//!
//! The fragment sorts at position 98 (after the 90-routing and
//! 95-rule-providers fragments) so the deep-merge order
//! remains: base → profile → 10..80 → 90 → 95 → 98.

use std::path::PathBuf;

/// The `config.d/98-profiles.yaml` fragment. Empty by default;
/// every snippet is commented so uncommenting produces a
/// self-validating config (validated at load time by the
/// `validate_profiles` step in `schema/validate.rs`).
pub(super) fn profiles_fragment() -> (PathBuf, String) {
    (
        PathBuf::from("config.d/98-profiles.yaml"),
        "# User-declared profile set. Each entry is one of:\n\
         #   - kind: remote   url + interval_minutes (>= 1, default 60)\n\
         #   - kind: local    path (relative to <config>/profiles/)\n\
         #   - kind: merge    parts: [id1, id2, ...] (deep-merge in order)\n\
         #\n\
         # Bodies are deep-merged on top of the base config and the\n\
         # `config.d/` fragments. The legacy `CALY_PROFILE` env var\n\
         # still selects the *active* profile from `profiles/<name>.yaml`.\n\
         #\n\
         # After uncommenting, run `caly profile refresh` to populate\n\
         # the on-disk cache at `<state>/profiles/<id>.yaml` for any\n\
         # `kind: remote` entry. Local entries are read directly from\n\
         # `<config>/profiles/<path>`.\n\
         profiles: []\n\
         \n\
         # Example profile set (uncomment to use):\n\
         # profiles:\n\
         #   - id: team-shared\n\
         #     name: Team shared rules\n\
         #     kind: remote\n\
         #     url: https://example.com/team.yaml\n\
         #     interval_minutes: 60\n\
         #   - id: local-extra\n\
         #     name: Local overrides\n\
         #     kind: local\n\
         #     path: extra.yaml\n\
         #   - id: combined\n\
         #     name: Team + local\n\
         #     kind: merge\n\
         #     parts: [team-shared, local-extra]\n"
            .to_owned(),
    )
}
