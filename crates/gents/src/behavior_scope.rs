//! Canonical names for newly authored behavior configuration.
//!
//! These names describe a behavior's configuration closure. They do not grant
//! ownership or authorization; those remain the responsibility of the
//! document's `agent_did`, `scope_behavior_id`, and DefraDB ACP.

use crate::Collection;
use std::fmt;

/// Stable pack-qualified identity for the built-in first-run configurator.
pub const SETUP_CONFIGURATOR_BEHAVIOR_ID: &str = "gents:base:configurator";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedNameError {
    InvalidBehaviorSlug(String),
}

impl fmt::Display for GeneratedNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBehaviorSlug(slug) => write!(
                formatter,
                "behavior slug {slug:?} must be a lowercase ASCII alphanumeric kebab segment"
            ),
        }
    }
}

impl std::error::Error for GeneratedNameError {}

/// A behavior-owned document path.
///
/// The nested compaction paths distinguish the optional summary inference
/// branch from the behavior's primary inference branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BehaviorComponentPath {
    Context,
    Tools,
    Inference,
    Sampling,
    Execution,
    RetryPolicy,
    Compaction,
    CompactionInference,
    CompactionSampling,
    CompactionExecution,
    CompactionRetryPolicy,
}

impl BehaviorComponentPath {
    pub const ALL: [Self; 11] = [
        Self::Context,
        Self::Tools,
        Self::Inference,
        Self::Sampling,
        Self::Execution,
        Self::RetryPolicy,
        Self::Compaction,
        Self::CompactionInference,
        Self::CompactionSampling,
        Self::CompactionExecution,
        Self::CompactionRetryPolicy,
    ];

    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Tools => "tools",
            Self::Inference => "inference",
            Self::Sampling => "sampling",
            Self::Execution => "execution",
            Self::RetryPolicy => "retry-policy",
            Self::Compaction => "compaction",
            Self::CompactionInference => "compaction:inference",
            Self::CompactionSampling => "compaction:sampling",
            Self::CompactionExecution => "compaction:execution",
            Self::CompactionRetryPolicy => "compaction:retry-policy",
        }
    }

    pub const fn collection(self) -> Collection {
        match self {
            Self::Context => Collection::AgentContext,
            Self::Tools => Collection::Tools,
            Self::Inference | Self::CompactionInference => Collection::InferenceProfile,
            Self::Sampling | Self::CompactionSampling => Collection::InferenceSampling,
            Self::Execution | Self::CompactionExecution => Collection::InferenceExecution,
            Self::RetryPolicy | Self::CompactionRetryPolicy => Collection::InferenceRetryPolicy,
            Self::Compaction => Collection::Compaction,
        }
    }
}

/// One collection-qualified logical key reserved by a generated behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservedBehaviorSlot {
    pub collection: Collection,
    pub logical_id: String,
}

/// Presentation text and the stable identity generated for a personal behavior.
///
/// `display_name` is retained verbatim and never participates in identity
/// derivation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedBehaviorNames {
    pub display_name: String,
    pub behavior_id: String,
}

impl GeneratedBehaviorNames {
    pub fn component_id(&self, component: BehaviorComponentPath) -> String {
        behavior_component_id(&self.behavior_id, component)
    }

    pub fn reserved_slots(&self) -> impl Iterator<Item = ReservedBehaviorSlot> + '_ {
        reserved_behavior_slots(&self.behavior_id)
    }
}

/// Returns whether `segment` is a nonempty lowercase ASCII alphanumeric kebab
/// segment. There is intentionally no length limit.
pub fn valid_kebab_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// Returns whether every colon-separated segment follows the generated-key
/// grammar.
pub fn valid_generated_qualified_key(key: &str) -> bool {
    !key.is_empty() && key.split(':').all(valid_kebab_segment)
}

/// Returns whether `key` is exactly a newly generated personal behavior key:
/// `local:{slug}`.
pub fn valid_new_personal_behavior_key(key: &str) -> bool {
    let mut segments = key.split(':');
    matches!(
        (segments.next(), segments.next(), segments.next()),
        (Some("local"), Some(slug), None) if valid_kebab_segment(slug)
    )
}

/// Allocates a slug for a collision ordinal. Ordinals zero and one both select
/// the unsuffixed slug, matching the formal contract; allocation iterators start
/// at one.
pub fn allocate_behavior_slug(slug: &str, ordinal: u64) -> Result<String, GeneratedNameError> {
    validate_behavior_slug(slug)?;
    Ok(format_behavior_slug(slug, ordinal))
}

fn format_behavior_slug(slug: &str, ordinal: u64) -> String {
    if ordinal < 2 {
        slug.to_owned()
    } else {
        format!("{slug}-{ordinal}")
    }
}

/// Generates a personal behavior key for a collision ordinal.
pub fn personal_behavior_key(slug: &str, ordinal: u64) -> Result<String, GeneratedNameError> {
    validate_behavior_slug(slug)?;
    Ok(format_personal_behavior_key(slug, ordinal))
}

fn format_personal_behavior_key(slug: &str, ordinal: u64) -> String {
    format!("local:{}", format_behavior_slug(slug, ordinal))
}

fn validate_behavior_slug(slug: &str) -> Result<(), GeneratedNameError> {
    valid_kebab_segment(slug)
        .then_some(())
        .ok_or_else(|| GeneratedNameError::InvalidBehaviorSlug(slug.to_owned()))
}

/// Generates the stable key independently of the retained display text.
pub fn generated_personal_behavior_names(
    display_name: impl Into<String>,
    slug: &str,
    ordinal: u64,
) -> Result<GeneratedBehaviorNames, GeneratedNameError> {
    Ok(GeneratedBehaviorNames {
        display_name: display_name.into(),
        behavior_id: personal_behavior_key(slug, ordinal)?,
    })
}

/// Derives a behavior-owned component key from its stable behavior key.
pub fn behavior_component_id(behavior_id: &str, component: BehaviorComponentPath) -> String {
    format!("{behavior_id}:{}", component.suffix())
}

/// Converts user-facing text into the canonical personal-behavior slug.
///
/// Runs of non-ASCII-alphanumeric characters collapse to one separator. When
/// the display text has no usable characters, the stable fallback keeps the
/// generated key valid while retaining the original display text separately.
pub fn behavior_slug_from_display_name(display_name: &str) -> String {
    let mut slug = String::new();
    let mut pending_separator = false;
    for character in display_name.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character.to_ascii_lowercase());
            pending_separator = false;
        } else if matches!(character, '\'' | '’') {
            // Apostrophes do not create a word boundary: "Jack's" becomes
            // "jacks", while the exact display text remains independent.
        } else if !slug.is_empty() {
            pending_separator = true;
        }
    }
    if slug.is_empty() {
        "behavior".to_owned()
    } else {
        slug
    }
}

/// Enumerates every collection-qualified key a fully materialized behavior may
/// occupy, including the behavior document itself. Optional components reserve
/// their deterministic slots even while absent.
pub fn reserved_behavior_slots(
    behavior_id: &str,
) -> impl Iterator<Item = ReservedBehaviorSlot> + '_ {
    std::iter::once(ReservedBehaviorSlot {
        collection: Collection::AgentBehavior,
        logical_id: behavior_id.to_owned(),
    })
    .chain(
        BehaviorComponentPath::ALL
            .into_iter()
            .map(move |component| ReservedBehaviorSlot {
                collection: component.collection(),
                logical_id: behavior_component_id(behavior_id, component),
            }),
    )
}

/// Infinite in practice and finite only at the integer representation boundary;
/// exhaustion is handled with checked increment rather than wrapping to a key
/// that was already considered.
#[derive(Debug, Clone)]
pub struct PersonalBehaviorKeyCandidates<'a> {
    slug: &'a str,
    next_ordinal: Option<u64>,
}

impl Iterator for PersonalBehaviorKeyCandidates<'_> {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        let ordinal = self.next_ordinal?;
        self.next_ordinal = ordinal.checked_add(1);
        Some(format_personal_behavior_key(self.slug, ordinal))
    }
}

/// Enumerates `local:{slug}`, `local:{slug}-2`, and so on without imposing a
/// name-length limit.
pub fn personal_behavior_key_candidates(
    slug: &str,
) -> Result<PersonalBehaviorKeyCandidates<'_>, GeneratedNameError> {
    validate_behavior_slug(slug)?;
    Ok(PersonalBehaviorKeyCandidates {
        slug,
        next_ordinal: Some(1),
    })
}

/// Finds the first candidate whose behavior and possible component slots are
/// all unoccupied. Collection qualification prevents unrelated document kinds
/// with the same logical key from producing false collisions.
pub fn find_available_personal_behavior_key(
    slug: &str,
    mut occupied: impl FnMut(Collection, &str) -> bool,
) -> Result<Option<String>, GeneratedNameError> {
    Ok(personal_behavior_key_candidates(slug)?.find(|behavior_id| {
        reserved_behavior_slots(behavior_id)
            .all(|slot| !occupied(slot.collection, &slot.logical_id))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_and_derivation_match_generated_lean_cases() {
        let cases = crate::lean_vocab_test::lean_configuration_naming_cases();

        for case in &cases.validation {
            assert_eq!(
                valid_generated_qualified_key(&case.key),
                case.valid,
                "{}",
                case.key
            );
        }

        for case in &cases.personal_allocations {
            assert_eq!(
                personal_behavior_key("reviewer", case.ordinal as u64).unwrap(),
                case.behavior_id
            );
        }

        assert_eq!(cases.components.len(), BehaviorComponentPath::ALL.len());
        for (case, component) in cases.components.iter().zip(BehaviorComponentPath::ALL) {
            assert_eq!(component.suffix(), case.suffix);
            assert_eq!(
                behavior_component_id("local:reviewer-2", component),
                case.id
            );
        }

        assert!(cases.display_name_independent);
        assert!(cases.legacy_stored_key_accepted);
        assert!(cases.new_personal_key_valid);
        assert!(valid_new_personal_behavior_key("local:reviewer-2"));
    }

    #[test]
    fn display_name_does_not_affect_identity() {
        let left = generated_personal_behavior_names("Jack's reviewer", "reviewer", 1).unwrap();
        let right = generated_personal_behavior_names("レビュー担当", "reviewer", 1).unwrap();

        assert_eq!(left.behavior_id, right.behavior_id);
        assert_ne!(left.display_name, right.display_name);
    }

    #[test]
    fn display_names_produce_valid_stable_slugs() {
        assert_eq!(
            behavior_slug_from_display_name("New behaviour"),
            "new-behaviour"
        );
        assert_eq!(
            behavior_slug_from_display_name(" Jack's  Reviewer! "),
            "jacks-reviewer"
        );
        assert_eq!(
            behavior_slug_from_display_name("Jack’s Reviewer"),
            "jacks-reviewer"
        );
        assert_eq!(behavior_slug_from_display_name("✨"), "behavior");
    }

    #[test]
    fn grammar_has_no_length_limit() {
        let segment = "a".repeat(100_000);
        assert!(valid_kebab_segment(&segment));
        assert!(valid_generated_qualified_key(&format!("local:{segment}")));
        assert!(personal_behavior_key(&segment, 1).is_ok());
    }

    #[test]
    fn public_personal_key_generators_reject_invalid_slugs() {
        for slug in [
            "",
            "Reviewer",
            "code_reviewer",
            "-reviewer",
            "reviewer--worker",
        ] {
            assert_eq!(
                personal_behavior_key(slug, 1),
                Err(GeneratedNameError::InvalidBehaviorSlug(slug.to_owned()))
            );
            assert!(allocate_behavior_slug(slug, 1).is_err());
            assert!(generated_personal_behavior_names("Display", slug, 1).is_err());
            assert!(personal_behavior_key_candidates(slug).is_err());
            assert!(find_available_personal_behavior_key(slug, |_, _| false).is_err());
        }
    }

    #[test]
    fn component_collections_match_the_owned_closure() {
        let expected = [
            Collection::AgentContext,
            Collection::Tools,
            Collection::InferenceProfile,
            Collection::InferenceSampling,
            Collection::InferenceExecution,
            Collection::InferenceRetryPolicy,
            Collection::Compaction,
            Collection::InferenceProfile,
            Collection::InferenceSampling,
            Collection::InferenceExecution,
            Collection::InferenceRetryPolicy,
        ];

        assert_eq!(
            BehaviorComponentPath::ALL.map(BehaviorComponentPath::collection),
            expected
        );
    }

    #[test]
    fn reserved_slots_include_behavior_and_every_component() {
        let slots: Vec<_> = reserved_behavior_slots("local:reviewer").collect();
        assert_eq!(slots.len(), 1 + BehaviorComponentPath::ALL.len());
        assert_eq!(slots[0].collection, Collection::AgentBehavior);
        assert_eq!(slots[0].logical_id, "local:reviewer");
        assert_eq!(
            slots.last().unwrap().logical_id,
            "local:reviewer:compaction:retry-policy"
        );
    }

    #[test]
    fn collision_allocation_checks_every_reserved_slot() {
        let occupied = [
            (Collection::Tools, "local:reviewer:tools"),
            (Collection::AgentBehavior, "local:reviewer-2"),
        ];

        assert_eq!(
            find_available_personal_behavior_key("reviewer", |collection, id| {
                occupied.contains(&(collection, id))
            }),
            Ok(Some("local:reviewer-3".to_owned()))
        );
    }

    #[test]
    fn collision_ordinal_uses_checked_increment() {
        let mut candidates = PersonalBehaviorKeyCandidates {
            slug: "reviewer",
            next_ordinal: Some(u64::MAX),
        };

        assert_eq!(
            candidates.next(),
            Some(format!("local:reviewer-{}", u64::MAX))
        );
        assert_eq!(candidates.next(), None);
    }
}
