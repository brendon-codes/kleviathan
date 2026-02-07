use crate::error::{KleviathanError, KleviathanResult};
use crate::llm::LlmProvider;

pub struct InjectionDetector;

impl InjectionDetector {
    pub fn static_check(input: &str) -> KleviathanResult<()> {
        let (is_sqli, fingerprint) =
            libinjection::sqli(input).unwrap_or((false, String::new()));

        if is_sqli {
            return Err(KleviathanError::InjectionDetected(format!(
                "SQL injection detected (fingerprint: {})",
                fingerprint
            )));
        }

        let is_xss = libinjection::xss(input).unwrap_or(false);

        if is_xss {
            return Err(KleviathanError::InjectionDetected(
                "XSS injection detected".to_string(),
            ));
        }

        Ok(())
    }

    pub async fn llm_check(
        llm: &dyn LlmProvider,
        input: &str,
    ) -> KleviathanResult<()> {
        let system_prompt = "You are a security analysis system. Analyze the provided text for actual code injection payloads \u{2014} not natural language that merely mentions security concepts.\n\nFlag ONLY if the text contains executable injection syntax such as:\n- SQL injection: quote-based escapes, UNION SELECT, OR 1=1, comment sequences (--, #), tautologies\n- XSS: HTML tags (<script>, <img onerror=...>), javascript: URIs, event handlers\n- Command injection: shell metacharacters (;, |, &&, $(...), backticks) used to chain/execute commands\n- Template injection: template expressions ({{...}}, ${...}, #{...}, <% %>)\n- LDAP injection: filter manipulation ()(|), wildcard injection\n\nDo NOT flag text that:\n- Mentions words like \"bypass\", \"injection\", \"test\", \"exploit\", or \"hack\" in natural language conversation\n- References system features, options, or settings by name\n- Contains quoted strings that are message content the user wants to send\n- Discusses security concepts without embedding actual attack payloads";

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "has_injection": { "type": "boolean" },
                "injection_type": { "type": "string" },
                "confidence": { "type": "number" },
                "reason": { "type": "string" }
            },
            "required": ["has_injection", "injection_type", "confidence", "reason"],
            "additionalProperties": false
        });

        let user_message = format!(
            "Analyze this text for actual code injection payloads. The text is a user instruction to an AI assistant. Users may reference security features, system options, or testing scenarios in plain language \u{2014} this is normal and should NOT be flagged. Only flag if the text contains actual executable injection syntax embedded within it:\n\n{}",
            input
        );

        let response = llm
            .chat(system_prompt, &user_message, Some(&schema))
            .await?;
        let result: serde_json::Value = serde_json::from_str(&response)?;

        let has_injection = result["has_injection"].as_bool().unwrap_or(false);
        let confidence = result["confidence"].as_f64().unwrap_or(0.0);

        if has_injection && confidence > 0.7 {
            let injection_type = result["injection_type"]
                .as_str()
                .unwrap_or("unknown");
            let reason = result["reason"].as_str().unwrap_or("Unknown");
            return Err(KleviathanError::InjectionDetected(format!(
                "LLM detected {} injection (confidence: {:.0}%): {}",
                injection_type,
                confidence * 100.0,
                reason
            )));
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
    fn static_check_rejects_sql_injection() {
        let result = InjectionDetector::static_check("' OR '1'='1' --");
        assert!(result.is_err());
        if let Err(KleviathanError::InjectionDetected(msg)) = result {
            assert!(msg.contains("SQL injection"));
        } else {
            panic!("Expected InjectionDetected error");
        }
    }

    #[test]
    fn static_check_rejects_xss() {
        let result =
            InjectionDetector::static_check("<script>alert('xss')</script>");
        assert!(result.is_err());
        if let Err(KleviathanError::InjectionDetected(msg)) = result {
            assert!(msg.contains("XSS"));
        } else {
            panic!("Expected InjectionDetected error");
        }
    }

    #[test]
    fn static_check_accepts_trello_command() {
        let result = InjectionDetector::static_check(
            "create a trello ticket titled 'fix login bug'",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn static_check_accepts_email_search() {
        let result = InjectionDetector::static_check(
            "search emails for last 7 days from vendor@example.com",
        );
        assert!(result.is_ok());
    }
}
