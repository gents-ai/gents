use std::time::Duration;

use anyhow::{bail, Context, Result};
use gents::inference::auth::credential::{resolve_oauth_credential, AccountPick};
use gents::inference::backends::setup::{
    connection_spec, model_option, InferenceModelOption, InferenceProviderId,
    RecommendedNumberControl, RecommendedReasoningControl,
};
use gents::inference::resolved::ReasoningEffort;

use crate::config_writes::ConfigAccess;
use crate::interactive_backend::{prompt_line, prompt_line_default, PickedConnection};

const MODELS_SHOWN: usize = 15;

/// What the desktop's inference setup saves for a connection. Values no one is asked about keep
/// the recommendation, as there.
#[derive(Debug, Default, PartialEq)]
pub(super) struct ModelChoice {
    pub(super) model_name: String,
    pub(super) reasoning_effort: Option<ReasoningEffort>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) context_window: Option<i64>,
    pub(super) max_output_tokens: Option<i64>,
    pub(super) max_concurrent: Option<i64>,
}

/// Runs after sign-in, because a sign-in connection can only list its models with the credential
/// that sign-in stores.
pub(super) async fn choose(
    access: &ConfigAccess,
    node_did: &str,
    connection: &PickedConnection,
) -> Result<ModelChoice> {
    let spec = connection_spec(connection.provider, connection.method, &connection.endpoint)?;
    let credential = match spec.oauth_provider {
        Some(provider) => {
            let credential =
                resolve_oauth_credential(access, node_did, provider, AccountPick::Reference(None))
                    .await?;
            if credential.is_none() {
                let model_name = connection
                    .fallback_model
                    .clone()
                    .context("a sign-in connection has a default model")?;
                return Ok(ModelChoice {
                    model_name,
                    ..ModelChoice::default()
                });
            }
            credential
        }
        None => None,
    };

    eprintln!("Looking for models...");
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(10))
        .build()?;
    let scanned = gents::discover_backend_models(
        &client,
        spec.provider_kind,
        &spec.endpoint,
        connection.api_key.as_deref(),
        credential.as_ref(),
    )
    .await;
    (connection.next_screen)();
    let options = match scanned {
        Ok(models) if !models.is_empty() => models
            .into_iter()
            .map(|model| model_option(connection.provider, connection.method, model))
            .collect::<Result<Vec<_>>>()?,
        Ok(_) => {
            eprintln!("The connection succeeded, but the provider advertised no models.");
            return type_model(connection).await;
        }
        Err(_) if connection.provider == InferenceProviderId::Local => {
            eprintln!(
                "No model answered at {}. Setup will continue, but agents cannot run until one does: start it, or change the address later with `gents config backend set`.",
                spec.endpoint
            );
            return type_model(connection).await;
        }
        Err(_) => {
            eprintln!(
                "Could not list {}'s models. Check the key and the connection.",
                connection.provider_name
            );
            return type_model(connection).await;
        }
    };
    let option = pick_model(connection, &options).await?;
    ask_values(connection, option).await
}

async fn type_model(connection: &PickedConnection) -> Result<ModelChoice> {
    let model_name = match &connection.fallback_model {
        Some(default) => prompt_line_default("Model", default).await?,
        None => prompt_line("Model name: ").await?.trim().to_string(),
    };
    if model_name.is_empty() {
        bail!("no model name entered");
    }
    (connection.next_screen)();
    Ok(ModelChoice {
        model_name,
        ..ModelChoice::default()
    })
}

#[derive(Debug, PartialEq)]
enum ModelAnswer {
    Chosen(usize),
    Narrowed(Vec<usize>),
    NoMatch,
}

/// A number picks from what is on screen; anything else searches every model, because a long
/// list shows only its start.
fn read_model_answer(answer: &str, names: &[&str], shown: &[usize]) -> ModelAnswer {
    let answer = answer.trim();
    if let Some(index) = answer
        .parse::<usize>()
        .ok()
        .filter(|number| *number >= 1)
        .and_then(|number| shown.iter().take(MODELS_SHOWN).nth(number - 1))
    {
        return ModelAnswer::Chosen(*index);
    }
    if let Some(index) = names.iter().position(|name| *name == answer) {
        return ModelAnswer::Chosen(index);
    }
    let wanted = answer.to_lowercase();
    let matches: Vec<usize> = (0..names.len())
        .filter(|index| names[*index].to_lowercase().contains(&wanted))
        .collect();
    match matches.as_slice() {
        [] => ModelAnswer::NoMatch,
        [only] => ModelAnswer::Chosen(*only),
        _ => ModelAnswer::Narrowed(matches),
    }
}

async fn pick_model<'a>(
    connection: &PickedConnection,
    options: &'a [InferenceModelOption],
) -> Result<&'a InferenceModelOption> {
    let names: Vec<&str> = options
        .iter()
        .map(|option| option.advertised.model_name.as_str())
        .collect();
    let default = connection
        .fallback_model
        .as_deref()
        .filter(|model| names.contains(model))
        .unwrap_or(names[0]);
    let all: Vec<usize> = (0..names.len()).collect();
    let mut shown = all.clone();
    loop {
        eprintln!(
            "Choose a model for {} ({} found)",
            connection.provider_name,
            names.len()
        );
        for (number, index) in (1..).zip(shown.iter().take(MODELS_SHOWN)) {
            eprintln!("  {number}) {}", names[*index]);
        }
        if shown.len() > MODELS_SHOWN {
            eprintln!("  ... and {} more", shown.len() - MODELS_SHOWN);
        }
        let answer = prompt_line_default("Number, or part of a name", default).await?;
        (connection.next_screen)();
        match read_model_answer(&answer, &names, &shown) {
            ModelAnswer::Chosen(index) => return Ok(&options[index]),
            ModelAnswer::Narrowed(matches) => shown = matches,
            ModelAnswer::NoMatch => {
                shown.clone_from(&all);
                eprintln!("No model matches that.");
            }
        }
    }
}

fn read_number(answer: &str, control: &RecommendedNumberControl) -> Option<f64> {
    let value = answer
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())?;
    (value >= control.min && control.max.is_none_or(|max| value <= max)).then_some(value)
}

fn read_effort(answer: &str, control: &RecommendedReasoningControl) -> Option<ReasoningEffort> {
    ReasoningEffort::parse(answer)
        .ok()
        .filter(|effort| control.choices.contains(effort))
}

async fn ask_number(label: &str, control: &RecommendedNumberControl) -> Result<f64> {
    let range = match control.max {
        Some(max) => format!("{} to {max}", control.min),
        None => format!("{} or more", control.min),
    };
    loop {
        let question = format!("{label} ({range})");
        let answer = prompt_line_default(&question, &control.recommended.to_string()).await?;
        match read_number(&answer, control) {
            Some(value) => return Ok(value),
            None => eprintln!("Answer a number, {range}."),
        }
    }
}

async fn ask_effort(control: &RecommendedReasoningControl) -> Result<ReasoningEffort> {
    let choices = control
        .choices
        .iter()
        .map(|effort| effort.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    loop {
        let question = format!("Reasoning effort ({choices})");
        let answer = prompt_line_default(&question, control.recommended.as_str()).await?;
        match read_effort(&answer, control) {
            Some(effort) => return Ok(effort),
            None => eprintln!("Answer one of {choices}."),
        }
    }
}

/// Asks what the desktop's model controls show, in their order, for the model's recommendation.
async fn ask_values(
    connection: &PickedConnection,
    option: &InferenceModelOption,
) -> Result<ModelChoice> {
    let model_name = option.advertised.model_name.clone();
    let recommendation = &option.recommendation;
    let intro =
        || eprintln!("Settings for {model_name}. Press enter to keep the value in brackets.");
    let mut choice = ModelChoice {
        model_name: model_name.clone(),
        context_window: recommendation
            .context_window
            .as_ref()
            .map(|c| c.recommended),
        max_output_tokens: recommendation
            .max_output_tokens
            .as_ref()
            .map(|c| c.recommended),
        max_concurrent: Some(recommendation.max_concurrent.recommended),
        ..ModelChoice::default()
    };
    if let Some(control) = &recommendation.reasoning_effort {
        intro();
        choice.reasoning_effort = Some(ask_effort(control).await?);
        (connection.next_screen)();
    }
    if let Some(control) = &recommendation.temperature {
        intro();
        choice.temperature = Some(ask_number("Temperature", control).await?);
        (connection.next_screen)();
    }
    if let Some(control) = &recommendation.top_p {
        intro();
        choice.top_p = Some(ask_number("Top-p", control).await?);
        (connection.next_screen)();
    }
    Ok(choice)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 3] = ["gpt-4", "gpt-5-mini", "claude-opus"];

    #[test]
    fn a_number_picks_from_what_is_on_screen() {
        let shown = [1, 2];
        assert_eq!(
            read_model_answer("1", &NAMES, &shown),
            ModelAnswer::Chosen(1)
        );
        assert_eq!(
            read_model_answer("2\n", &NAMES, &shown),
            ModelAnswer::Chosen(2)
        );
    }

    #[test]
    fn a_number_past_the_list_searches_instead() {
        let shown = [0, 1, 2];
        assert_eq!(
            read_model_answer("4", &NAMES, &shown),
            ModelAnswer::Chosen(0)
        );
        assert_eq!(read_model_answer("9", &NAMES, &shown), ModelAnswer::NoMatch);
        assert_eq!(read_model_answer("0", &NAMES, &shown), ModelAnswer::NoMatch);
    }

    #[test]
    fn a_number_cannot_reach_past_the_rows_shown() {
        let names: Vec<String> = (0..MODELS_SHOWN + 3).map(|n| format!("m{n}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let shown: Vec<usize> = (0..names.len()).collect();
        let past = (MODELS_SHOWN + 1).to_string();
        assert_ne!(
            read_model_answer(&past, &names, &shown),
            ModelAnswer::Chosen(MODELS_SHOWN)
        );
    }

    #[test]
    fn a_name_or_part_of_one_searches_every_model() {
        let shown = [0];
        assert_eq!(
            read_model_answer("claude-opus", &NAMES, &shown),
            ModelAnswer::Chosen(2)
        );
        assert_eq!(
            read_model_answer("OPUS", &NAMES, &shown),
            ModelAnswer::Chosen(2)
        );
        assert_eq!(
            read_model_answer("gpt", &NAMES, &shown),
            ModelAnswer::Narrowed(vec![0, 1])
        );
        assert_eq!(
            read_model_answer("nope", &NAMES, &shown),
            ModelAnswer::NoMatch
        );
    }

    #[test]
    fn a_number_answer_must_be_in_range() {
        let bounded = RecommendedNumberControl {
            recommended: 0.7,
            min: 0.0,
            max: Some(2.0),
            step: 0.05,
        };
        assert_eq!(read_number("0.2", &bounded), Some(0.2));
        assert_eq!(read_number(" 2 ", &bounded), Some(2.0));
        assert_eq!(read_number("0", &bounded), Some(0.0));
        for wrong in ["2.1", "-1", "hot", "", "NaN", "inf"] {
            assert_eq!(read_number(wrong, &bounded), None, "{wrong:?}");
        }
        let open = RecommendedNumberControl {
            max: None,
            ..bounded
        };
        assert_eq!(read_number("100", &open), Some(100.0));
    }

    #[test]
    fn an_effort_answer_must_be_one_the_model_offers() {
        let control = RecommendedReasoningControl {
            recommended: ReasoningEffort::Medium,
            choices: vec![ReasoningEffort::Low, ReasoningEffort::Medium],
        };
        assert_eq!(read_effort("LOW", &control), Some(ReasoningEffort::Low));
        assert_eq!(read_effort("high", &control), None);
        assert_eq!(read_effort("loud", &control), None);
    }
}
