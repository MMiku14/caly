//! Deterministic display-name and derived-tag normalization.

use caly_domain::{NodeDisplayName, NodeTag, NodeTags, TextError, sanitized_display_name};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NormalizeError {
    Text(TextError),
    TagCapacity,
}

impl From<TextError> for NormalizeError {
    fn from(value: TextError) -> Self {
        Self::Text(value)
    }
}

/// Collapses Unicode whitespace and rejects an empty/oversized result.
pub fn normalize_display_name(value: &str) -> Result<NodeDisplayName, NormalizeError> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    sanitized_display_name(normalized).map_err(NormalizeError::Text)
}

/// Derives stable protocol and best-effort region tags from normalized name.
pub fn derive_tags(name: &NodeDisplayName, protocol: &str) -> Result<NodeTags, NormalizeError> {
    let mut tags = NodeTags::new();
    push_tag(&mut tags, format!("protocol:{protocol}"))?;
    let lower = name.as_str().to_ascii_lowercase();
    if let Some(region) = region_tag(&lower) {
        push_tag(&mut tags, format!("region:{region}"))?;
    }
    Ok(tags)
}

fn push_tag(tags: &mut NodeTags, value: String) -> Result<(), NormalizeError> {
    let tag = NodeTag::new(value)?;
    if tags.iter().any(|existing| existing == &tag) {
        return Ok(());
    }
    tags.try_push(tag).map_err(|_| NormalizeError::TagCapacity)
}

fn region_tag(value: &str) -> Option<&'static str> {
    let regions = [
        ("hk", ["香港", "hong kong", " hk "]),
        ("jp", ["日本", "japan", "tokyo"]),
        ("sg", ["新加坡", "singapore", " sg "]),
        ("us", ["美国", "united states", "los angeles"]),
        ("tw", ["台湾", "taiwan", "taipei"]),
    ];
    regions
        .iter()
        .find(|(_, markers)| markers.iter().any(|marker| value.contains(marker)))
        .map(|(region, _)| *region)
}
