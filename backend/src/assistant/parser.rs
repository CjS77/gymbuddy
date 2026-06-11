use super::actions::AssistantResponse;

/// Extract a JSON object from LLM output that may contain markdown fencing
/// or surrounding prose. Prioritizes `{...}` objects over `[...]` arrays
/// since our response format is always a JSON object.
fn extract_json_object(text: &str) -> &str {
    let trimmed = text.trim();

    // Strip ```json ... ``` fences
    if let Some(start) = trimmed.find("```json") {
        let after_fence = &trimmed[start + 7..];
        if let Some(end) = after_fence.find("```") {
            return after_fence[..end].trim();
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after_fence = &trimmed[start + 3..];
        if let Some(end) = after_fence.find("```") {
            return after_fence[..end].trim();
        }
    }

    // Find outermost { } first (our response is always an object)
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }

    trimmed
}

/// Parse an LLM response string into an `AssistantResponse`.
///
/// Handles markdown-fenced JSON, raw JSON, and falls back to treating the
/// entire response as a plain-text message with no actions.
pub fn parse_assistant_response(raw: &str) -> AssistantResponse {
    let json_str = extract_json_object(raw);
    tracing::debug!(raw_len = raw.len(), json_len = json_str.len(), "Parsing LLM response");
    tracing::debug!(json = json_str, "Extracted JSON");

    match serde_json::from_str::<AssistantResponse>(json_str) {
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
