use rustrict::{Censor, Type};

use crate::error::{KleviathanError, KleviathanResult};
use crate::llm::LlmProvider;

pub struct AbuseDetector;

impl AbuseDetector {
    pub fn static_check(input: &str) -> KleviathanResult<()> {
        let mut censor = Censor::from_str(input);
        let analysis = censor.analyze();

        let dominated_types = Type::PROFANE | Type::SEXUAL | Type::OFFENSIVE | Type::MEAN;
        if analysis.is(dominated_types) {
            return Err(KleviathanError::AbuseDetected(
                "Abusive language detected in static check".to_string(),
            ));
        }

        Ok(())
    }

    pub async fn llm_check(
        llm: &dyn LlmProvider,
        input: &str,
    ) -> KleviathanResult<()> {
        let system_prompt = "You are a content safety classifier. Analyze the provided text for abusive language including hate speech, harassment, threats, slurs, and profanity. Consider context and intent.";

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "is_abusive": { "type": "boolean" },
                "confidence": { "type": "number" },
                "reason": { "type": "string" }
            },
            "required": ["is_abusive", "confidence", "reason"],
            "additionalProperties": false
        });

        let user_message = format!(
            "Analyze this text for abusive content. Be thorough but avoid false positives on normal conversation:\n\n{}",
            input
        );

        let response = llm.chat(system_prompt, &user_message, Some(&schema)).await?;
        let result: serde_json::Value = serde_json::from_str(&response)?;

        let is_abusive = result["is_abusive"].as_bool().unwrap_or(false);
        let confidence = result["confidence"].as_f64().unwrap_or(0.0);

        if is_abusive && confidence > 0.7 {
            let reason = result["reason"].as_str().unwrap_or("Unknown");
            return Err(KleviathanError::AbuseDetected(
                format!(
                    "LLM detected abusive language (confidence: {:.0}%): {}",
                    confidence * 100.0,
                    reason
                ),
            ));
        }

        Ok(())
    }

    pub async fn check(
        llm: &dyn LlmProvider,
        input: &str,
    ) -> KleviathanResult<()> {
        Self::static_check(input)?;
        Self::llm_check(llm, input).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_check_rejects_profane_input() {
        let result = AbuseDetector::static_check("fuck you");
        assert!(result.is_err());
        if let Err(KleviathanError::AbuseDetected(msg)) = result {
            assert!(msg.contains("static check"));
        } else {
            panic!("Expected AbuseDetected error");
        }
    }

    #[test]
    fn static_check_rejects_leetspeak_evasion() {
        let result = AbuseDetector::static_check("f u c k");
        assert!(result.is_err());
    }

    #[test]
    fn static_check_accepts_clean_input() {
        let result = AbuseDetector::static_check("Hello, how are you today?");
        assert!(result.is_ok());
    }

    #[test]
    fn static_check_accepts_normal_conversation() {
        let result = AbuseDetector::static_check("Can you help me with my project?");
        assert!(result.is_ok());
    }
}
