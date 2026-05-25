pub const GEMINI_URL: &str =
    "https://api.dataforseo.com/v3/ai_optimization/gemini/llm_scraper/live/advanced";
pub const CHATGPT_URL: &str =
    "https://api.dataforseo.com/v3/ai_optimization/chat_gpt/llm_scraper/live/advanced";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Provider {
    Gemini,
    ChatGpt,
    Both,
}

impl Provider {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "gemini" => Some(Self::Gemini),
            "chatgpt" => Some(Self::ChatGpt),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn url(&self) -> &'static str {
        match self {
            Self::Gemini => GEMINI_URL,
            Self::ChatGpt => CHATGPT_URL,
            Self::Both => panic!("url() called on Provider::Both — use individual providers"),
        }
    }

    pub fn supports_force_web_search(&self) -> bool {
        match self {
            Self::Gemini => false,
            Self::ChatGpt => true,
            Self::Both => true,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Gemini => "gemini",
            Self::ChatGpt => "chatgpt",
            Self::Both => "both",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_gemini_lowercase() {
        assert!(matches!(
            Provider::from_str("gemini"),
            Some(Provider::Gemini)
        ));
    }

    #[test]
    fn from_str_gemini_uppercase() {
        assert!(matches!(
            Provider::from_str("GEMINI"),
            Some(Provider::Gemini)
        ));
    }

    #[test]
    fn from_str_gemini_mixedcase() {
        assert!(matches!(
            Provider::from_str("Gemini"),
            Some(Provider::Gemini)
        ));
    }

    #[test]
    fn from_str_chatgpt_lowercase() {
        assert!(matches!(
            Provider::from_str("chatgpt"),
            Some(Provider::ChatGpt)
        ));
    }

    #[test]
    fn from_str_chatgpt_uppercase() {
        assert!(matches!(
            Provider::from_str("CHATGPT"),
            Some(Provider::ChatGpt)
        ));
    }

    #[test]
    fn from_str_both() {
        assert!(matches!(Provider::from_str("both"), Some(Provider::Both)));
    }

    #[test]
    fn from_str_both_uppercase() {
        assert!(matches!(Provider::from_str("BOTH"), Some(Provider::Both)));
    }

    #[test]
    fn from_str_invalid() {
        assert!(Provider::from_str("perplexity").is_none());
    }

    #[test]
    fn from_str_empty() {
        assert!(Provider::from_str("").is_none());
    }

    #[test]
    fn gemini_url_contains_gemini() {
        assert!(Provider::Gemini.url().contains("gemini"));
        assert!(Provider::Gemini.url().starts_with("https://"));
    }

    #[test]
    fn chatgpt_url_contains_chat_gpt() {
        assert!(Provider::ChatGpt.url().contains("chat_gpt"));
        assert!(Provider::ChatGpt.url().starts_with("https://"));
    }

    #[test]
    #[should_panic]
    fn both_url_panics() {
        Provider::Both.url();
    }

    #[test]
    fn gemini_no_force_web_search() {
        assert!(!Provider::Gemini.supports_force_web_search());
    }

    #[test]
    fn chatgpt_supports_force_web_search() {
        assert!(Provider::ChatGpt.supports_force_web_search());
    }

    #[test]
    fn both_supports_force_web_search() {
        assert!(Provider::Both.supports_force_web_search());
    }

    #[test]
    fn names_are_lowercase() {
        assert_eq!(Provider::Gemini.name(), "gemini");
        assert_eq!(Provider::ChatGpt.name(), "chatgpt");
        assert_eq!(Provider::Both.name(), "both");
    }
}
