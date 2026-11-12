use crate::generated::*;

pub use gents_protocol::request_input::QueueDelivery as LeanTitleQueueDelivery;
pub use gents_protocol::request_input::QueuePosition as LeanTitleQueuePosition;

impl From<LeanRequestPurpose> for gents_protocol::request_admission::RequestPurpose {
    fn from(purpose: LeanRequestPurpose) -> Self {
        match purpose {
            LeanRequestPurpose::Normal => Self::Normal,
            LeanRequestPurpose::TitleAudit => Self::TitleAudit,
            LeanRequestPurpose::AccessReview => Self::AccessReview,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use crate::{required_nullable, LeanRequestPurpose};

    #[test]
    fn generated_title_groups_decode_without_native_binding_claim() {
        use super::super::{
            lean_title_admission_join_cases, lean_title_request_admission_cases,
            lean_title_usage_cases,
        };

        assert!(!lean_title_request_admission_cases().is_empty());
        assert!(!lean_title_usage_cases().is_empty());
        assert!(!lean_title_admission_join_cases().is_empty());
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RequiredNullableControl {
        purpose: LeanRequestPurpose,
        #[serde(deserialize_with = "required_nullable")]
        value: Option<String>,
    }

    #[test]
    fn required_nullable_and_purpose_are_not_missing_field_defaults() {
        assert!(
            serde_json::from_str::<RequiredNullableControl>(r#"{"purpose":"normal"}"#).is_err()
        );
        assert!(serde_json::from_str::<RequiredNullableControl>(r#"{"value":null}"#).is_err());
        let decoded: RequiredNullableControl =
            serde_json::from_str(r#"{"purpose":"title-audit","value":null}"#).unwrap();
        assert_eq!(decoded.purpose, LeanRequestPurpose::TitleAudit);
        assert!(decoded.value.is_none());
        assert!(serde_json::from_str::<RequiredNullableControl>(
            r#"{"purpose":"titleAudit","value":null}"#
        )
        .is_err());
    }
}
