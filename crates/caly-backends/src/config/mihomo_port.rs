//! `ConfigActorPort` transaction for the Mihomo backend.
//!
//! Split out of `backends/config/mod.rs` (audit #70 file-length
//! budget): prepare (render + real-binary validation staging),
//! commit (atomic publish + generation trim) and rollback for one
//! Mihomo destination file.

use caly_platform::fs::{atomic_write, AtomicFileContents, AtomicWritePlan};
use caly_ports::{ActorFailure, CommittedConfig, ConfigActorPort, ConfigCandidate, PreparedConfig};
use std::path::PathBuf;

use super::failure;
use super::resolve_tun_from_config;
use super::MihomoConfigBackend;

/// Strips a trailing `# caly-generation: N` marker line (the renderer's
/// only comment) from a published config so a no-op comparison ignores it.
/// Any trailing marker is stripped — the renderer never emits comments, so
/// this cannot collide with a config body (2026-08-12 agent audit).
fn strip_generation_marker(existing: &str) -> &str {
    let Some(line_start) = existing.rfind("\n# caly-generation: ") else {
        return existing;
    };
    let marker = &existing[line_start + 1..];
    if marker
        .strip_prefix("# caly-generation: ")
        .is_some_and(|rest| {
            let digits = rest.trim_end_matches(['\r', '\n']);
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        })
    {
        &existing[..line_start]
    } else {
        existing
    }
}

impl ConfigActorPort for MihomoConfigBackend {
    fn parse_and_render(
        &mut self,
        candidate: ConfigCandidate,
    ) -> Result<PreparedConfig, ActorFailure> {
        // Re-read the TUN tuning per apply (like the subscription URL re-reads
        // on refresh): a `tun.enabled` edit takes effect without a restart.
        self.tun = resolve_tun_from_config();
        let settings = self.build_settings()?;
        let contents = self.renderer.render(&settings).map_err(|error| {
            failure(
                &format!("Mihomo config render failed: {error}"),
                "inspect config generation",
            )
        })?;
        let generation = self.generation.saturating_add(1);
        // Validate against the real binary before the candidate is considered
        // prepared, so a rejected config can never reach commit.
        if let Some(binary) = self.binary.clone() {
            let validation_path = super::mihomo_backend::stage_validation(
                "Mihomo",
                "yaml",
                &mut self.filesystem,
                &self.workdir,
                &contents,
                generation,
            )?;
            let report = self.validate_config(
                binary,
                self.workdir.clone(),
                validation_path.clone(),
                generation,
            )?;
            let _ = std::fs::remove_file(&validation_path);
            let _ = std::fs::remove_file(
                self.workdir
                    .join(format!("config.validate.{generation}.tmp")),
            );
            super::mihomo_backend::ensure_accepted("Mihomo", report)?;
        }
        self.prepared.insert(candidate.id, (generation, contents));
        Ok(PreparedConfig {
            candidate_id: candidate.id,
            generation,
        })
    }

    fn discard_prepared(&mut self, prepared: PreparedConfig) -> Result<(), ActorFailure> {
        self.prepared.remove(&prepared.candidate_id);
        Ok(())
    }

    fn commit_candidate(
        &mut self,
        prepared: PreparedConfig,
    ) -> Result<CommittedConfig, ActorFailure> {
        let (_, contents) = self
            .prepared
            .remove(&prepared.candidate_id)
            .ok_or_else(|| failure("prepared config is missing", "re-render the candidate"))?;
        // 刀 4 (2026-08-12 pipeline design): a render that is byte-identical
        // to the already-published config skips the write and the caller
        // skips the kernel restart — a no-op apply must not drop
        // connections. The published file appends `# caly-generation: N`,
        // so the comparison strips that trailing marker (which carries the
        // PREVIOUS generation) before diffing the render body.
        let unchanged = std::fs::read_to_string(&self.destination)
            .ok()
            .is_some_and(|existing| {
                // The published file appends `# caly-generation: N`. Strip
                // ANY trailing marker line (the renderer never emits
                // comments, so this cannot collide with a config body) and
                // normalize trailing whitespace/CRLF, so a daemon restart
                // (generation resets to 0) or an editor's line endings do
                // not silently defeat the no-op optimization
                // (2026-08-12 agent audit).
                // Compare normalized bodies: both sides trim trailing
                // whitespace so a trailing newline difference (or a CRLF
                // editor round-trip) cannot defeat the no-op detection.
                let existing_clean = strip_generation_marker(&existing);
                existing_clean.trim_end() == String::from_utf8_lossy(contents.as_slice()).trim_end()
            });
        if unchanged {
            return Ok(CommittedConfig {
                candidate_id: prepared.candidate_id,
                generation: self.generation,
                unchanged: true,
            });
        }
        // Ensure the owner-only destination directory exists before atomic publish.
        if let Some(parent) = self.destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                failure(
                    &format!("cannot create config directory: {error}"),
                    "inspect config filesystem ownership",
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
        let mut bytes = contents.into_vec();
        bytes.extend_from_slice(format!("# caly-generation: {}\n", prepared.generation).as_bytes());
        let contents = AtomicFileContents::try_from_vec(bytes).map_err(|_| {
            failure(
                "Mihomo generation is too large",
                "reduce generated configuration",
            )
        })?;
        super::mihomo_backend::publish_and_record(
            &mut self.filesystem,
            &self.destination,
            contents,
            prepared.generation,
            &mut self.history,
            "Mihomo",
        )?;
        self.generation = prepared.generation;
        Ok(CommittedConfig {
            candidate_id: prepared.candidate_id,
            generation: prepared.generation,
            unchanged: false,
        })
    }

    fn current_contents(&self) -> Option<Vec<u8>> {
        self.history
            .iter()
            .next_back()
            .map(|(_, contents)| contents.clone().into_vec())
    }

    fn rollback_commit(&mut self, committed: CommittedConfig) -> Result<(), ActorFailure> {
        // A no-op apply never wrote anything, so there is nothing to roll
        // back (2026-08-12 agent audit).
        if committed.unchanged {
            return Ok(());
        }
        if self.generation != committed.generation {
            return Ok(());
        }
        let previous = self
            .history
            .range(..committed.generation)
            .next_back()
            .map(|(generation, contents)| (*generation, contents.clone()));
        if let Some((generation, contents)) = previous {
            let temporary = PathBuf::from(format!(
                "{}.rollback.{}",
                self.destination.display(),
                generation
            ));
            atomic_write(
                &mut self.filesystem,
                AtomicWritePlan {
                    destination: self.destination.clone(),
                    temporary,
                    contents,
                },
            )
            .map_err(|error| {
                failure(
                    &format!("config rollback failed: {error}"),
                    "inspect generation filesystem ownership",
                )
            })?;
            self.generation = generation;
            // 刀 6 (boundary audit): the rolled-back generation must not
            // stay in history — current_contents() would serve a bad
            // generation for a hot reload.
            self.history.remove(&committed.generation);
        }
        Ok(())
    }
}
