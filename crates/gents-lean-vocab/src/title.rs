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
