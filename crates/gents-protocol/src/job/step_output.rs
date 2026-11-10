use serde::{Deserialize, Serialize};

use super::{
    DataWrite, JobControlRequest, JobEffects, JobFailure, JobGuestPin, JobProgress, JobResume,
    JobScratch, JobSuspend,
};

/// The output of one job step: exactly one of `result`, `fail`, `suspend` or
/// `effects`, plus that variant's sibling riders. Serde's external tagging
/// cannot carry the riders, so `Deserialize` and `Serialize` are hand-written.
///
/// `pin` on `Result` or `Suspend` is accepted on step 0 only, and `resume` only
/// beside it; the output is then delivered inline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStepOutput {
    Result {
        result: serde_json::Value,
        writes: Vec<DataWrite>,
        scratch: Vec<JobScratch>,
        progress: Option<JobProgress>,
        control: Vec<JobControlRequest>,
        pin: Option<JobGuestPin>,
        resume: Option<JobResume>,
    },
    Fail {
        fail: JobFailure,
        writes: Vec<DataWrite>,
        progress: Option<JobProgress>,
    },
    Suspend {
        suspend: JobSuspend,
        writes: Vec<DataWrite>,
        scratch: Vec<JobScratch>,
        progress: Option<JobProgress>,
        pin: Option<JobGuestPin>,
        resume: Option<JobResume>,
        state: Option<serde_json::Value>,
    },
    Effects {
        effects: JobEffects,
    },
}

const STEP_OUTPUT_DISCRIMINATORS: [&str; 4] = ["result", "fail", "suspend", "effects"];

type StepObject = serde_json::Map<String, serde_json::Value>;

fn rider<T>(object: &mut StepObject, key: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned + Default,
{
    match object.remove(key) {
        None | Some(serde_json::Value::Null) => Ok(T::default()),
        Some(value) => serde_json::from_value(value).map_err(|e| format!("`{key}`: {e}")),
    }
}

fn required<T>(object: &mut StepObject, key: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let value = object.remove(key).unwrap_or(serde_json::Value::Null);
    serde_json::from_value(value).map_err(|e| format!("`{key}`: {e}"))
}

fn step_output_variant(kind: &str, o: &mut StepObject) -> Result<JobStepOutput, String> {
    Ok(match kind {
        "result" => JobStepOutput::Result {
            result: o.remove("result").unwrap_or_default(),
            writes: rider(o, "writes")?,
            scratch: rider(o, "scratch")?,
            progress: rider(o, "progress")?,
            control: rider(o, "control")?,
            pin: rider(o, "pin")?,
            resume: rider(o, "resume")?,
        },
        "fail" => JobStepOutput::Fail {
            fail: required(o, "fail")?,
            writes: rider(o, "writes")?,
            progress: rider(o, "progress")?,
        },
        "suspend" => JobStepOutput::Suspend {
            suspend: required(o, "suspend")?,
            writes: rider(o, "writes")?,
            scratch: rider(o, "scratch")?,
            progress: rider(o, "progress")?,
            pin: rider(o, "pin")?,
            resume: rider(o, "resume")?,
            state: rider(o, "state")?,
        },
        _ => JobStepOutput::Effects {
            effects: required(o, "effects")?,
        },
    })
}

impl<'de> Deserialize<'de> for JobStepOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        const SHAPE: &str =
            "a JSON object with exactly one of `result`, `fail`, `suspend` or `effects`";
        let serde_json::Value::Object(mut object) = serde_json::Value::deserialize(deserializer)?
        else {
            return Err(D::Error::custom(format!("expected {SHAPE}")));
        };
        let present: Vec<&str> = STEP_OUTPUT_DISCRIMINATORS
            .into_iter()
            .filter(|key| object.contains_key(*key))
            .collect();
        let [kind] = present[..] else {
            return Err(D::Error::custom(format!(
                "expected {SHAPE}, found {} of them",
                present.len()
            )));
        };
        let riders: &[&str] = match kind {
            "result" => &["writes", "scratch", "progress", "control", "pin", "resume"],
            "fail" => &["writes", "progress"],
            "suspend" => &["writes", "scratch", "progress", "pin", "resume", "state"],
            _ => &[],
        };
        if let Some(unknown) = object
            .keys()
            .find(|key| key.as_str() != kind && !riders.contains(&key.as_str()))
        {
            return Err(D::Error::custom(format!(
                "unknown key `{unknown}` beside `{kind}`; expected {SHAPE}, and `{kind}` accepts only the riders {riders:?}"
            )));
        }
        step_output_variant(kind, &mut object).map_err(D::Error::custom)
    }
}

impl Serialize for JobStepOutput {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        fn list<M, T>(m: &mut M, key: &str, v: &[T]) -> Result<(), M::Error>
        where
            M: SerializeMap,
            T: Serialize,
        {
            if v.is_empty() {
                Ok(())
            } else {
                m.serialize_entry(key, v)
            }
        }
        fn opt<M, T>(m: &mut M, key: &str, v: &Option<T>) -> Result<(), M::Error>
        where
            M: SerializeMap,
            T: Serialize,
        {
            match v {
                Some(v) => m.serialize_entry(key, v),
                None => Ok(()),
            }
        }
        let mut m = serializer.serialize_map(None)?;
        match self {
            Self::Result {
                result,
                writes,
                scratch,
                progress,
                control,
                pin,
                resume,
            } => {
                m.serialize_entry("result", result)?;
                list(&mut m, "writes", writes)?;
                list(&mut m, "scratch", scratch)?;
                opt(&mut m, "progress", progress)?;
                list(&mut m, "control", control)?;
                opt(&mut m, "pin", pin)?;
                opt(&mut m, "resume", resume)?;
            }
            Self::Fail {
                fail,
                writes,
                progress,
            } => {
                m.serialize_entry("fail", fail)?;
                list(&mut m, "writes", writes)?;
                opt(&mut m, "progress", progress)?;
            }
            Self::Suspend {
                suspend,
                writes,
                scratch,
                progress,
                pin,
                resume,
                state,
            } => {
                m.serialize_entry("suspend", suspend)?;
                list(&mut m, "writes", writes)?;
                list(&mut m, "scratch", scratch)?;
                opt(&mut m, "progress", progress)?;
                opt(&mut m, "pin", pin)?;
                opt(&mut m, "resume", resume)?;
                opt(&mut m, "state", state)?;
            }
            Self::Effects { effects } => m.serialize_entry("effects", effects)?,
        }
        m.end()
    }
}
