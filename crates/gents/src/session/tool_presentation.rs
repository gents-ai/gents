use anyhow::Result;
use gents_protocol::output::PayloadPresentation;

pub(crate) fn render_presentation(
    source: &str,
    presentation: &PayloadPresentation,
) -> Result<String> {
    match presentation {
        PayloadPresentation::Full => Ok(source.to_owned()),
        PayloadPresentation::Composed { parts } => {
            let mut rendered = String::new();
            for part in parts {
                match part {
                    gents_protocol::output::PresentationPart::Literal { text } => {
                        rendered.push_str(text)
                    }
                    gents_protocol::output::PresentationPart::OutputRange {
                        start_byte,
                        end_byte,
                    } => {
                        let start = usize::try_from(*start_byte)?;
                        let end = usize::try_from(*end_byte)?;
                        anyhow::ensure!(
                            start <= end
                                && end <= source.len()
                                && source.is_char_boundary(start)
                                && source.is_char_boundary(end),
                            "tool presentation range is not a UTF-8 source boundary"
                        );
                        rendered.push_str(&source[start..end]);
                    }
                }
            }
            Ok(rendered)
        }
    }
}
