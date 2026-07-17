use super::actions::AssistantResponse;

/// Extract the JSON object from raw LLM output.
///
/// This tolerance is the DELIBERATE, load-bearing strategy for reliable structured
/// output — not a lucky accident. The current provider does not negotiate a JSON
/// response format with the model (the `json_mode` flag on `LlmRequest` is ignored
/// by `corre`'s `OpenAiCompatProvider`; see the note in `handler::call_llm_with`),
/// so the "respond with ONLY a JSON object" prompt contract is actually recovered
/// HERE, by peeling off the wrapping models habitually add:
///   - markdown code fences (```` ```json … ``` ```` or bare ```` ``` … ``` ````), and
///   - surrounding prose ("Sure! Here you go: { … } hope that helps").
///
/// Our response schema is always a JSON *object*, so once any fence is stripped we
/// narrow to the outermost `{ … }` and ignore stray `[…]` or prose on either side.
/// Extraction is best-effort and never errors; callers that need a strict yes/no on
/// whether the model actually produced a valid response use
/// [`try_parse_assistant_response`].
fn extract_json_object(text: &str) -> &str {
    let trimmed = text.trim();

    // If the model fenced its reply, continue inside the first fenced block.
    let unfenced = strip_code_fence(trimmed).unwrap_or(trimmed);

    // The schema is a single object: narrow to the outermost braces.
    match (unfenced.find('{'), unfenced.rfind('}')) {
        (Some(start), Some(end)) if start <= end => &unfenced[start..=end],
        _ => unfenced,
    }
}

/// Return the trimmed contents of the first ```` ```json ```` (preferred) or bare
/// ```` ``` ```` fenced block. Returns `None` when there is no opening or no closing
/// fence, leaving the caller to work with the unfenced text.
fn strip_code_fence(trimmed: &str) -> Option<&str> {
    let after_open =
        trimmed.find("```json").map(|i| &trimmed[i + 7..]).or_else(|| trimmed.find("```").map(|i| &trimmed[i + 3..]))?;
    let end = after_open.find("```")?;
    Some(after_open[..end].trim())
}

/// Strictly parse raw LLM output into an [`AssistantResponse`], returning the serde
/// error when the extracted text is not a valid response.
///
/// This is the honest yes/no: it distinguishes "the model produced a real structured
/// response" from "the model produced prose". [`parse_assistant_response`] wraps this
/// with the tolerant plain-text fallback the free-form chat path relies on; callers
/// with a hard structured-output requirement can use this directly to fail loudly.
pub fn try_parse_assistant_response(raw: &str) -> Result<AssistantResponse, serde_json::Error> {
    serde_json::from_str::<AssistantResponse>(extract_json_object(raw))
}

/// Parse raw LLM output into an [`AssistantResponse`], tolerating the wrapping models
/// add (markdown fences, surrounding prose) and, as a last resort, degrading to a
/// plain-text message with no actions.
///
/// The tolerance here is intentional and load-bearing (see [`extract_json_object`]):
/// because the provider does not enforce a JSON response format, this is where the
/// prompt's "ONLY a JSON object" contract is recovered. The plain-text fallback is
/// the RIGHT behaviour for a free-form chat turn — a conversational reply is shown
/// as-is rather than erroring. But callers with a hard structured-output requirement
/// (e.g. the `/nextworkout` designer) must NOT treat that fallback as success: they
/// check for the specific action they demanded and fail loudly when it is absent,
/// instead of rendering the fallback prose as if it were the result.
pub fn parse_assistant_response(raw: &str) -> AssistantResponse {
    match try_parse_assistant_response(raw) {
        Ok(parsed) => {
            tracing::debug!(message_len = parsed.message.len(), actions = parsed.actions.len(), "Parsed response");
            for (i, action) in parsed.actions.iter().enumerate() {
                tracing::debug!(index = i, action = ?action, "Action from LLM");
            }
            parsed
        }
        Err(e) => {
            tracing::warn!(raw = raw, "Failed to parse LLM response as JSON: {e}");
            AssistantResponse { message: raw.to_string(), actions: vec![] }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::actions::AssistantAction;

    #[test]
    fn parse_well_formed_json() {
        let raw = r#"{"message": "Got it!", "actions": [{"type": "start_session"}]}"#;
        let resp = parse_assistant_response(raw);
        assert_eq!(resp.message, "Got it!");
        assert_eq!(resp.actions.len(), 1);
        assert!(matches!(resp.actions[0], AssistantAction::StartSession { .. }));
    }

    #[test]
    fn parse_with_markdown_fences() {
        let raw = "```json\n{\"message\": \"Done!\", \"actions\": []}\n```";
        let resp = parse_assistant_response(raw);
        assert_eq!(resp.message, "Done!");
        assert!(resp.actions.is_empty());
    }

    #[test]
    fn parse_with_bare_fences() {
        let raw = "```\n{\"message\": \"Hello\", \"actions\": []}\n```";
        let resp = parse_assistant_response(raw);
        assert_eq!(resp.message, "Hello");
    }

    #[test]
    fn parse_multiple_actions() {
        let raw = r#"{"message": "Logged bench and started session", "actions": [
            {"type": "start_session"},
            {"type": "log_exercise", "exercise": "Barbell Bench Press", "reps": 8, "weight_kg": 80.0}
        ]}"#;
        let resp = parse_assistant_response(raw);
        assert_eq!(resp.actions.len(), 2);
    }

    #[test]
    fn fallback_on_malformed_json() {
        let raw = "I'm not sure what you mean. Could you try again?";
        let resp = parse_assistant_response(raw);
        assert_eq!(resp.message, raw);
        assert!(resp.actions.is_empty());
    }

    #[test]
    fn extracts_json_from_surrounding_prose_and_fences() {
        // Fenced JSON with chatty prose on both sides — the shape models love to emit.
        let raw = "Sure, here's the plan!\n```json\n{\"message\": \"Ready\", \"actions\": [{\"type\": \"start_session\"}]}\n```\nHope that helps.";
        let resp = parse_assistant_response(raw);
        assert_eq!(resp.message, "Ready");
        assert_eq!(resp.actions.len(), 1);
        assert!(matches!(resp.actions[0], AssistantAction::StartSession { .. }));

        // A bare object embedded in prose, no fences at all.
        let raw = "Here you go: {\"message\": \"Hi\", \"actions\": []} -- anything else?";
        let resp = parse_assistant_response(raw);
        assert_eq!(resp.message, "Hi");
        assert!(resp.actions.is_empty());
    }

    #[test]
    fn non_json_input_is_reported_as_parse_failure() {
        let raw = "I'm sorry, I really can't help with that request.";
        // The strict parser REPORTS the failure rather than silently swallowing it...
        assert!(try_parse_assistant_response(raw).is_err());
        // ...and the tolerant wrapper degrades to a plain message with NO fabricated actions.
        let resp = parse_assistant_response(raw);
        assert_eq!(resp.message, raw);
        assert!(resp.actions.is_empty());
    }

    #[test]
    fn unknown_action_preserved_in_array() {
        let raw = r#"{"message": "Ok", "actions": [
            {"type": "unknown_thing"},
            {"type": "start_session"}
        ]}"#;
        let resp = parse_assistant_response(raw);
        assert_eq!(resp.actions.len(), 2);
        assert!(matches!(resp.actions[0], AssistantAction::Unknown));
        assert!(matches!(resp.actions[1], AssistantAction::StartSession { .. }));
    }

    #[test]
    fn null_actions_falls_back() {
        let raw = r#"{"message": "Hello!", "actions": null}"#;
        let resp = parse_assistant_response(raw);
        // null actions causes serde error, so fallback kicks in
        assert_eq!(resp.message, raw);
        assert!(resp.actions.is_empty());
    }

    #[test]
    fn absent_actions_field() {
        let raw = r#"{"message": "Just chatting"}"#;
        let resp = parse_assistant_response(raw);
        assert_eq!(resp.message, "Just chatting");
        assert!(resp.actions.is_empty());
    }
}
