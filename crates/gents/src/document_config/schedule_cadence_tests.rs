use crate::document_config::ScheduleCadence;
use crate::schedule_cron::CronMissedRunPolicy;

#[test]
fn interval_cadence_requires_a_positive_value() {
    for interval_secs in [i64::MIN, -1, 0] {
        assert!(ScheduleCadence::Interval { interval_secs }
            .validate()
            .is_err());
    }
    for interval_secs in [1, 60, i64::MAX] {
        ScheduleCadence::Interval { interval_secs }
            .validate()
            .expect("positive interval");
    }
}

#[test]
fn cron_cadence_uses_existing_expression_timezone_and_policy_rules() {
    for missed_run_policy in [None, Some(CronMissedRunPolicy::LatestOnly)] {
        ScheduleCadence::Cron {
            expression: "*/5 * * * *".into(),
            timezone: "America/Los_Angeles".into(),
            missed_run_policy,
        }
        .validate()
        .expect("supported cron policy");
    }
    for (expression, timezone) in [
        ("", "UTC"),
        ("  ", "UTC"),
        ("not cron", "UTC"),
        ("61 * * * *", "UTC"),
        ("* * * * *", ""),
        ("* * * * *", "  "),
        ("* * * * *", "No/Such_Zone"),
    ] {
        assert!(
            ScheduleCadence::Cron {
                expression: expression.into(),
                timezone: timezone.into(),
                missed_run_policy: None,
            }
            .validate()
            .is_err(),
            "invalid cadence accepted: {expression:?} {timezone:?}"
        );
    }
}
