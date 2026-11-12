use reqwest::blocking::{Client, Response, multipart};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::ports::{AudioBuffer, KikuError, PostProcessor, STTProvider};

const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";

const CLEAN_MODE_SYSTEM_PROMPT: &str = "\
You are a speech transcript cleaner. Remove filler words (such as 'um', 'uh', 'er', 'ah', 'like', 'you know', 'I mean', 'sort of', 'kind of' when used as fillers) and false starts or stutters. \
Fix punctuation and capitalization. \
Preserve the speaker's exact wording, tone, and meaning. Do not summarize, paraphrase, or add commentary. \
Output only the cleaned transcript text.";

struct GroqEndpoint {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl GroqEndpoint {
    fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, reqwest::Error> {
        let client = Client::builder().timeout(timeout).build()?;
        Ok(Self {
            client,
            api_key: api_key.into(),
            model: model.into(),
            base_url: GROQ_BASE_URL.to_owned(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    fn check_response(
        resp: Response,
        label: &str,
        make_err: fn(String) -> KikuError,
    ) -> Result<Response, KikuError> {
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(make_err(format!("{label} HTTP {status}: {body}")));
        }
        Ok(resp)
    }
}

/// Groq Whisper STT Provider (`whisper-large-v3-turbo` by default).
pub struct GroqSttProvider {
    endpoint: GroqEndpoint,
}

impl GroqSttProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self, KikuError> {
        let endpoint = GroqEndpoint::new(api_key, model, Duration::from_secs(30))
            .map_err(|e| KikuError::Stt(format!("failed to build HTTP client: {e}")))?;
        Ok(Self { endpoint })
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.endpoint.base_url = base_url.into();
        self
    }
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

impl STTProvider for GroqSttProvider {
    fn transcribe(&self, audio: &AudioBuffer) -> Result<String, KikuError> {
        let part = multipart::Part::bytes(audio.bytes.clone())
            .file_name(audio.format.filename())
            .mime_str(audio.format.mime_type())
            .map_err(|e| KikuError::Stt(format!("invalid audio mime type: {e}")))?;

        let form = multipart::Form::new()
            .part("file", part)
            .text("model", self.endpoint.model.clone())
            .text("response_format", "json");

        let resp = self
            .endpoint
            .client
            .post(self.endpoint.url("/audio/transcriptions"))
            .bearer_auth(&self.endpoint.api_key)
            .multipart(form)
            .send()
            .map_err(|e| KikuError::Stt(format!("Groq STT request failed: {e}")))?;

        let resp = GroqEndpoint::check_response(resp, "Groq STT", KikuError::Stt)?;
        let parsed: TranscriptionResponse = resp
            .json()
            .map_err(|e| KikuError::Stt(format!("invalid Groq STT JSON: {e}")))?;
        Ok(parsed.text)
    }
}

/// Groq LLM Post-Processor (`llama-3.1-8b-instant` by default) in Clean mode.
pub struct GroqPostProcessor {
    endpoint: GroqEndpoint,
}

impl GroqPostProcessor {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self, KikuError> {
        let endpoint = GroqEndpoint::new(api_key, model, Duration::from_secs(15))
            .map_err(|e| KikuError::PostProcess(format!("failed to build HTTP client: {e}")))?;
        Ok(Self { endpoint })
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.endpoint.base_url = base_url.into();
        self
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    temperature: f32,
    messages: [ChatMessage<'a>; 2],
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

impl PostProcessor for GroqPostProcessor {
    fn clean(&self, raw_transcript: &str) -> Result<String, KikuError> {
        if raw_transcript.trim().is_empty() {
            return Ok(String::new());
        }

        let payload = ChatCompletionRequest {
            model: &self.endpoint.model,
            temperature: 0.0,
            messages: [
                ChatMessage {
                    role: "system",
                    content: CLEAN_MODE_SYSTEM_PROMPT,
                },
                ChatMessage {
                    role: "user",
                    content: raw_transcript,
                },
            ],
        };

        let resp = self
            .endpoint
            .client
            .post(self.endpoint.url("/chat/completions"))
            .bearer_auth(&self.endpoint.api_key)
            .json(&payload)
            .send()
            .map_err(|e| {
                KikuError::PostProcess(format!("Groq post-process request failed: {e}"))
            })?;

        let resp = GroqEndpoint::check_response(resp, "Groq post-process", KikuError::PostProcess)?;
        let parsed: ChatCompletionResponse = resp
            .json()
            .map_err(|e| KikuError::PostProcess(format!("invalid Groq post-process JSON: {e}")))?;
        let text = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content.trim().to_owned())
            .ok_or_else(|| KikuError::PostProcess("Groq returned empty choices".to_owned()))?;
        Ok(text)
    }
}
