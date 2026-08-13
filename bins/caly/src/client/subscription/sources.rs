//! Subscription source writers (`set sub add/remove/enable/disable`).
//!
//! Split out of `client/subscription.rs` (audit #70 file-length
//! budget). Every writer round-trips the `subscriptions.sources`
//! list through the shared
//! [`super::super::super::yaml_surgery::edit_yaml_list`] primitive, so
//! comments and unknown keys elsewhere in `config.yaml` survive
//! and an idempotent call produces no disk write (audit #15/#16).

use caly_platform::paths::AppPaths;
use caly_profile::schema::SubscriptionSource;

use super::super::resource_writer::{
    ListEditOutcome, ResourceError, config_yaml_path, is_http_url,
};
use super::{MAX_SOURCE_NAME_CHARS, SubCmdError, SubWriteOutcome, load_declared_with};

/// Validates the optional `--name` display name. Trimming is
/// applied by the caller's write path; here we only reject the
/// unusable shapes (empty / over-long / control characters) and
/// hand the trimmed name back for storage.
fn validate_source_name(name: &str) -> Result<String, SubCmdError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(SubCmdError::InvalidName("name is empty".to_owned()));
    }
    if trimmed.chars().count() > MAX_SOURCE_NAME_CHARS {
        return Err(SubCmdError::InvalidName(format!(
            "name exceeds {MAX_SOURCE_NAME_CHARS} characters"
        )));
    }
    // A name containing `://` would be shadowed by the URL addressing
    // branch in `resolve_source_ref` and become unaddressable by name
    // (2026-08-12 CLI audit) — reject the shape at the writer.
    if trimmed.contains("://") {
        return Err(SubCmdError::InvalidName(
            "name must not contain `://` (it would be parsed as a URL)".to_owned(),
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(SubCmdError::InvalidName(
            "name contains control characters".to_owned(),
        ));
    }
    Ok(trimmed.to_owned())
}

/// W2-β2a (Q5): how the `sub add <url-or-file>` token resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    /// A trimmed `http(s)://` URL.
    Http,
    /// A local file, stored as a canonical `file://` URL so the
    /// fetch half (`caly-subscription::fetch_pinned`) needs no
    /// guessing about the on-disk location.
    File,
}

/// The canonical form written into `subscriptions.sources[].url`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedSource {
    pub url: String,
    pub kind: SourceKind,
}

/// W2-β2a (Q5): the default per-source refresh cadence for URL
/// sources when `--every` is not given — 24h in minutes. File
/// sources are pinned to `Some(0)` (static) instead.
pub const DEFAULT_REFRESH_EVERY_MINUTES: u64 = 24 * 60;

/// Normalizes the `set sub add` positional token (Q5: URLs and
/// files auto-detected):
///
/// - `http(s)://…` passes through trimmed ([`is_http_url`] keeps
///   the SSRF-side guard for the other writers);
/// - `file://…` requires the referenced file to exist;
/// - any other `…://…` scheme is rejected;
/// - anything else is treated as a local path: a leading `~` is
///   expanded by hand (no new dependency for one prefix), the path
///   must exist, and the stored form is the canonicalized absolute
///   `file://` URL. A missing file raises the §8-7 three-line
///   [`SubCmdError::SourceFileNotFound`].
pub fn normalize_source_token(raw: &str) -> Result<NormalizedSource, SubCmdError> {
    use std::borrow::Cow;
    use std::path::Path;
    let trimmed = raw.trim();
    if let Ok(url) = is_http_url(trimmed) {
        return Ok(NormalizedSource {
            url: url.to_owned(),
            kind: SourceKind::Http,
        });
    }
    if trimmed.is_empty() {
        return Err(SubCmdError::InvalidUrl(
            "source argument is empty (pass an http(s):// URL or a local file path)".to_owned(),
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("file://") {
        return file_source_from_path(trimmed, Path::new(rest));
    }
    if trimmed.contains("://") {
        return Err(SubCmdError::InvalidUrl(format!(
            "unsupported scheme `{trimmed}`; only http(s):// URLs and local files are supported"
        )));
    }
    let expanded: Cow<'_, str> = if trimmed == "~" {
        std::env::var("HOME").map_or(Cow::Borrowed(trimmed), Cow::Owned)
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        std::env::var("HOME").map_or(Cow::Borrowed(trimmed), |home| {
            Cow::Owned(format!("{home}/{rest}"))
        })
    } else {
        Cow::Borrowed(trimmed)
    };
    file_source_from_path(trimmed, Path::new(expanded.as_ref()))
}

/// Shared tail of [`normalize_source_token`]: existence check →
/// the §8-7 error, canonicalize → `file://` URL.
fn file_source_from_path(
    given: &str,
    path: &std::path::Path,
) -> Result<NormalizedSource, SubCmdError> {
    let resolved = if path.is_absolute() {
        path.display().to_string()
    } else {
        std::env::current_dir().map_or_else(
            |_| path.display().to_string(),
            |cwd| cwd.join(path).display().to_string(),
        )
    };
    let canonical = std::fs::canonicalize(path).map_err(|_| SubCmdError::SourceFileNotFound {
        given: given.to_owned(),
        resolved: resolved.clone(),
    })?;
    let url = url::Url::from_file_path(&canonical).map_err(|()| {
        SubCmdError::InvalidUrl(format!(
            "could not form a file:// URL from `{given}` (non-UTF-8 path)"
        ))
    })?;
    Ok(NormalizedSource {
        url: url.to_string(),
        kind: SourceKind::File,
    })
}

/// W2-β2b: resolves the `<name-or-url>` addressing token shared by
/// `sub set` / `sub refresh` / `sub remove` / `sub enable|disable`
/// into the source's stored URL.
///
/// Ruling: URL-shaped tokens (`http(s)://`, `file://`) address by
/// exact URL match; any other `…://…` token keeps the SSRF-shaped
/// `InvalidUrl` rejection; everything else is an exact display-name
/// match — zero hits falls through to the id interpretation, more
/// than one (hand-edited configs can duplicate names; `add` refuses
/// duplicates) is `AmbiguousName` (usage class → exit 2). A bare
/// integer addresses by the 1-based add-order id shown in `sub list`'s
/// ID column: the legacy scalar `subscriptions.url` counts as id 1
/// when present, sources follow in declaration (add) order, so the
/// id is stable until a source is removed.
pub fn resolve_source_ref(paths: &AppPaths, token: &str) -> Result<String, SubCmdError> {
    let trimmed = token.trim();
    let config = load_declared_with(paths)?;
    let sources = &config.subscriptions.sources;
    let url_shaped = is_http_url(trimmed).is_ok() || trimmed.starts_with("file://");
    if url_shaped {
        // The legacy scalar `subscriptions.url` is addressable by its
        // URL token too (2026-08-12 CLI audit: only the id 1 mapping
        // worked before).
        if sources.iter().any(|s| s.url == trimmed)
            || config.subscriptions.url.as_deref() == Some(trimmed)
        {
            return Ok(trimmed.to_owned());
        }
        return Err(SubCmdError::NotDeclared(trimmed.to_owned()));
    }
    if trimmed.is_empty() {
        return Err(SubCmdError::InvalidUrl(
            "source argument is empty (pass an id, a name, an http(s):// URL, or a file)"
                .to_owned(),
        ));
    }
    if trimmed.contains("://") {
        return Err(SubCmdError::InvalidUrl(format!(
            "unsupported scheme `{trimmed}`; only http(s):// URLs and files are supported"
        )));
    }
    // Exact display-name match wins over the id interpretation, so a
    // source legitimately named `1` stays addressable by name.
    let hits: Vec<&str> = sources
        .iter()
        .filter(|s| s.name.as_deref() == Some(trimmed))
        .map(|s| s.url.as_str())
        .collect();
    match hits.as_slice() {
        [] => {}
        [one] => return Ok((*one).to_owned()),
        _ => return Err(SubCmdError::AmbiguousName(trimmed.to_owned())),
    }
    // Id addressing: same 1-based order `sub list` displays (legacy
    // scalar first, then sources in declaration order).
    if let Ok(id) = trimmed.parse::<usize>() {
        if id == 0 {
            return Err(SubCmdError::InvalidUrl(
                "source id starts at 1; pass the id shown in `caly sub list`".to_owned(),
            ));
        }
        let mut index = id.saturating_sub(1);
        if let Some(legacy_url) = config.subscriptions.url.as_deref() {
            if index == 0 {
                return Ok(legacy_url.to_owned());
            }
            index -= 1;
        }
        if let Some(source) = sources.get(index) {
            return Ok(source.url.clone());
        }
        let total = sources.len() + usize::from(config.subscriptions.url.is_some());
        return Err(SubCmdError::NotDeclared(format!(
            "source id {id} is out of range; `caly sub list` shows {total} source(s)"
        )));
    }
    Err(SubCmdError::NotDeclared(trimmed.to_owned()))
}

/// W2-β2b (`sub remove --purge`): deletes the daemon-owned cached
/// body for `url` — `$state/subscriptions/<hex(subscription id)>`
/// (plus the writer's `.tmp` sidecar). Best-effort by contract: a
/// missing file is fine (daemon never cached, or the operator
/// cleared it), an IO failure warns on stderr without turning a
/// successful config write into a failure.
fn purge_cached_body(paths: &AppPaths, url: &str) {
    let id = caly_backends::subscription::subscription_id_for_url(url);
    let hex = crate::client::hex(id.into_bytes());
    let dir = paths.state.join("subscriptions");
    for candidate in [dir.join(&hex), dir.join(format!("{hex}.tmp"))] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!(
                "warning: could not purge cached body {}: {error}",
                candidate.display()
            ),
        }
    }
}

pub fn add_source(
    paths: &AppPaths,
    token: &str,
    name: Option<&str>,
    refresh_every_minutes: Option<u64>,
    apply: bool,
) -> Result<SubWriteOutcome, SubCmdError> {
    let source = normalize_source_token(token)?;
    // Q5 cadence ruling: files are always static (`Some(0)`); a
    // non-zero `--every` on a file is a usage error. URLs get the
    // explicit value, or the 24h default when the flag is absent;
    // an explicit `--every 0` pins a URL source as static too.
    let refresh_every_minutes = match (source.kind, refresh_every_minutes) {
        (SourceKind::File, Some(minutes)) if minutes > 0 => {
            return Err(SubCmdError::EveryOnFile);
        }
        (SourceKind::File, _) => Some(0),
        (SourceKind::Http, Some(minutes)) => Some(minutes),
        (SourceKind::Http, None) => Some(DEFAULT_REFRESH_EVERY_MINUTES),
    };
    let name = name.map(validate_source_name).transpose()?;
    let config = load_declared_with(paths)?;
    if config
        .subscriptions
        .sources
        .iter()
        .any(|s| s.url == source.url)
    {
        return Err(SubCmdError::AlreadyDeclared(source.url));
    }
    // §8-4: display names must be unique across sources, otherwise
    // `sub refresh <name>` / `sub set <name>` (β2b) can't address a
    // single entry.
    if let Some(new_name) = name.as_deref()
        && config
            .subscriptions
            .sources
            .iter()
            .any(|s| s.name.as_deref() == Some(new_name))
    {
        return Err(SubCmdError::NameTaken(new_name.to_owned()));
    }
    if !apply {
        return Ok(SubWriteOutcome::DryRun);
    }
    let url = source.url;
    mutate_subscription_sources(paths, |sources| {
        sources.push(SubscriptionSource {
            url,
            enabled: true,
            name,
            refresh_every_minutes,
        });
        Ok(())
    })?;
    Ok(SubWriteOutcome::Applied)
}

/// W2-β2b (`caly sub set <name-or-url> --url U | --name N |
/// --every H`): edits one declared source in place. At least one
/// change flag is guaranteed at the clap layer (required ArgGroup);
/// the writer re-validates everything so a library caller can't
/// produce an off-contract state either.
///
/// Rulings (same window as `add`, Q5):
/// - `--url` normalizes through [`normalize_source_token`];
///   colliding with another source's URL is `AlreadyDeclared`.
/// - `--name` runs the §8-4 uniqueness check excluding self.
/// - File sources are always pinned static: setting `--every > 0`
///   on one (addressed directly or via `--url` conversion) is
///   `EveryOnFile`; converting to a file re-pins `Some(0)`.
/// - All-folded-identical edits surface `NoChange` without a write.
pub fn set_source(
    paths: &AppPaths,
    target: &str,
    new_url: Option<&str>,
    new_name: Option<&str>,
    refresh_every_minutes: Option<u64>,
    apply: bool,
) -> Result<SubWriteOutcome, SubCmdError> {
    let current_url = resolve_source_ref(paths, target)?;
    let normalized = new_url.map(normalize_source_token).transpose()?;
    let new_name = new_name.map(validate_source_name).transpose()?;
    let config = load_declared_with(paths)?;
    let sources = &config.subscriptions.sources;
    if let Some(normalized) = &normalized
        && normalized.url != current_url
        && sources.iter().any(|s| s.url == normalized.url)
    {
        return Err(SubCmdError::AlreadyDeclared(normalized.url.clone()));
    }
    if let Some(name) = &new_name
        && sources
            .iter()
            .any(|s| s.url != current_url && s.name.as_deref() == Some(name))
    {
        return Err(SubCmdError::NameTaken(name.clone()));
    }
    let target_is_file = normalized.as_ref().map_or_else(
        || current_url.starts_with("file://"),
        |n| n.kind == SourceKind::File,
    );
    let final_every = if target_is_file {
        match refresh_every_minutes {
            Some(minutes) if minutes > 0 => return Err(SubCmdError::EveryOnFile),
            _ => Some(0),
        }
    } else {
        let current_every = sources
            .iter()
            .find(|s| s.url == current_url)
            .and_then(|s| s.refresh_every_minutes);
        refresh_every_minutes.or(current_every)
    };
    if !apply {
        return Ok(SubWriteOutcome::DryRun);
    }
    let outcome = mutate_subscription_sources(paths, |sources| {
        for source in sources.iter_mut() {
            if source.url == current_url {
                if let Some(normalized) = &normalized {
                    source.url.clone_from(&normalized.url);
                }
                if let Some(name) = &new_name {
                    source.name = Some(name.clone());
                }
                source.refresh_every_minutes = final_every;
                return Ok(());
            }
        }
        Err(ResourceError::NotDeclared(current_url))
    })?;
    Ok(outcome.write_outcome())
}

/// `set sub remove <name-or-url> [--purge] [--apply]`. Filters the
/// `sources` list; `target` resolves through
/// [`resolve_source_ref`] (W2-β2b: name or URL). Returns
/// `SubCmdError::NotDeclared` when the token addresses nothing.
/// `--purge` additionally deletes the daemon-owned cached body.
pub fn remove_source(
    paths: &AppPaths,
    target: &str,
    purge: bool,
    apply: bool,
) -> Result<SubWriteOutcome, SubCmdError> {
    let url = resolve_source_ref(paths, target)?;
    let config = load_declared_with(paths)?;
    // The legacy scalar `subscriptions.url` (id 1) is not part of the
    // `sources` list; `remove` cannot delete it, and purging its cache
    // would silently starve a still-declared source (2026-08-12 audit).
    if config.subscriptions.url.as_deref() == Some(url.as_str()) {
        return Err(SubCmdError::InvalidUrl(
            "the legacy `subscriptions.url` cannot be removed via `sub`; edit config.yaml to drop it".to_owned(),
        ));
    }
    if !apply {
        return Ok(SubWriteOutcome::DryRun);
    }
    let mut removed = false;
    mutate_subscription_sources(paths, |sources| {
        let before = sources.len();
        sources.retain(|s| s.url != url);
        removed = before != sources.len();
        Ok(())
    })?;
    // Purge only when the entry was actually deleted: a no-op remove
    // must not discard a live cache body.
    if purge && removed {
        purge_cached_body(paths, &url);
    }
    Ok(SubWriteOutcome::Applied)
}

/// W2-β2b (`caly sub enable <name-or-url> [--apply]`). Flips
/// `enabled: true` on the matching source. (All sources
/// are enabled by default, so this is a no-op when the
/// source is already enabled — but the writer accepts
/// it for symmetry with `disable`.) `target` resolves
/// through [`resolve_source_ref`].
pub fn enable_source(
    paths: &AppPaths,
    target: &str,
    apply: bool,
) -> Result<SubWriteOutcome, SubCmdError> {
    set_source_enabled(paths, target, true, apply)
}

/// W2-β2b (`caly sub disable <name-or-url> [--apply]`). Flips
/// `enabled: false` on the matching source.
pub fn disable_source(
    paths: &AppPaths,
    target: &str,
    apply: bool,
) -> Result<SubWriteOutcome, SubCmdError> {
    set_source_enabled(paths, target, false, apply)
}

fn set_source_enabled(
    paths: &AppPaths,
    url: &str,
    enabled: bool,
    apply: bool,
) -> Result<SubWriteOutcome, SubCmdError> {
    // Round 27 (debug): the dry-run path was a
    // silent no-op for unknown URLs. Pre-Round 27
    // `set sub enable <unknown-url>` returned
    // `DryRun` (success) for `--dry-run` but
    // `NotDeclared` (error) for `--apply`, so an
    // operator who ran the dry-run as a safety
    // check saw an OK envelope and confidently
    // ran `--apply`, only to find the call
    // actually failed. The dry-run is now
    // existence-checked the same way `apply`
    // is: an unknown URL surfaces `NotDeclared`
    // regardless of `apply`, mirroring the
    // `add_source` (already-declared check on
    // both paths) and `remove_source`
    // (not-declared check on both paths)
    // contracts. The `trimmed` value is also the
    // `url` shape that lands in the on-disk
    // `SubscriptionSource` (the writer stores
    // `trimmed`, not the raw `url`), so a
    // whitespace-padded user input lands on disk
    // and is matched against the same trimmed
    // form on subsequent `enable` / `disable`
    // calls. W2-β2b: the existence check moved into the shared
    // [`resolve_source_ref`] addressing layer (name or URL); the
    // dry-run vs apply parity contract is unchanged.
    let trimmed = resolve_source_ref(paths, url)?;
    if !apply {
        return Ok(SubWriteOutcome::DryRun);
    }
    // Round 26: the apply path routes through the shared
    // `mutate_subscription_sources` helper. The
    // `NoChange` short-circuit (Round 25) now lives in
    // the helper itself (audit #16): setting the flag to
    // its current value produces an identical typed
    // before/after pair, so the helper returns `NoChange`
    // before any backup or write happens — an idempotent
    // `set sub enable` no longer touches the disk at all.
    let outcome = mutate_subscription_sources(paths, |sources| {
        for source in sources.iter_mut() {
            if source.url == trimmed {
                source.enabled = enabled;
                return Ok(());
            }
        }
        // The URL was present in the pre-mutate
        // `load_declared_with` but absent in the
        // mutate-time read — a concurrent
        // modification race. The original
        // `NotDeclared` error is the right
        // contract here; surface it through the
        // helper's error channel.
        Err(ResourceError::NotDeclared(trimmed.clone()))
    })?;
    Ok(outcome.write_outcome())
}

// ── Internal helpers (struct-typed write paths) ────────

/// Round 26: the single mutation primitive for the
/// `subscriptions.sources` Vec. The pre-Round-26
/// `add_source` / `remove_source` /
/// `set_source_enabled` writers each inlined the
/// same 6-step recipe (load → check → mutate →
/// backup → write → validate), with 4 subtle
/// drift points: the dry-run path used bespoke
/// `read_yaml_value` (which `mutate_list` already
/// has under a different name), the post-write
/// validation lived in `write_yaml_value` (which
/// `mutate_list` has as
/// `parse_and_validate_yaml`), and the
/// per-writer inline `write_subscriptions` /
/// `write_yaml_value` pairs duplicated the typed
/// pull / typed render / atomic-rename
/// triple. The helper here unifies the 4-step
/// recipe so each writer is a 5-line shim
/// around a `FnOnce(&mut Vec<SubscriptionSource>)`
/// closure, and the validation / backup /
/// atomic-rename contract lives in one place.
///
/// Audit #15/#16: the helper now writes through
/// [`super::super::yaml_surgery::edit_yaml_list`] — a
/// targeted line edit of the `sources:` block
/// that preserves comments / unknown keys /
/// formatting elsewhere in the file and no
/// longer materialises the `subscriptions:`
/// scalar defaults — and an idempotent mutation
/// short-circuits as [`ListEditOutcome::NoChange`]
/// before any backup or write runs.
///
/// The closure returns `Result<(), ResourceError>`
/// so the writer's domain-specific
/// `AlreadyDeclared` / `NotDeclared` /
/// `Invalid(_)` variants round-trip through the
/// `From<ResourceError>` impl on `SubCmdError`.
fn mutate_subscription_sources<F>(
    paths: &AppPaths,
    mutate: F,
) -> Result<ListEditOutcome, SubCmdError>
where
    F: FnOnce(&mut Vec<SubscriptionSource>) -> Result<(), ResourceError>,
{
    let path = config_yaml_path(paths);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        SubCmdError::Shared(ResourceError::Write(format!(
            "cannot read {}: {error}",
            path.display()
        )))
    })?;
    // Audit #15/#16: the write is a targeted line edit of the
    // `subscriptions.sources` block (comments / unknown keys /
    // formatting elsewhere in the file survive; the scalar defaults
    // of `subscriptions:` are no longer materialised), and a no-op
    // mutation returns `NoChange` *before* the backup / write
    // pipeline runs, so an idempotent call produces no disk write.
    let outcome =
        super::super::yaml_surgery::edit_yaml_list::<SubscriptionSource, _, ResourceError>(
            &text,
            &["subscriptions", "sources"],
            ResourceError::Parse,
            |current| {
                let mut next = current.to_vec();
                mutate(&mut next)?;
                Ok(next)
            },
        )
        .map_err(SubCmdError::Shared)?;
    let super::super::yaml_surgery::EditOutcome::Changed(new_text) = outcome else {
        return Ok(ListEditOutcome::NoChange);
    };
    // Validate the new file *before* writing it:
    // a failed validation never reaches disk.
    if let Err(error) = caly_profile::schema::parse_and_validate_yaml(new_text.as_bytes()) {
        return Err(SubCmdError::Shared(ResourceError::Validate(format!(
            "post-write validation failed for {}: {error}",
            path.display()
        ))));
    }
    super::super::config_writer::backup_config_yaml(&path)?;
    super::super::config_writer::write_atomic(&path, &new_text)
        .map_err(|error| SubCmdError::Shared(ResourceError::Write(error.to_string())))?;
    Ok(ListEditOutcome::Changed)
}
