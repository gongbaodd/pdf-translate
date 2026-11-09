//! Serde types for the v1 line-delimited JSON protocol emitted by
//! `scripts/translate_pdf.py --events json` and
//! `scripts/translation_bridge.ts login|status --events json`.

use serde::Deserialize;
use serde_json::Value;

/// Events from `translate_pdf.py --events json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobEvent {
    Config {
        config: Value,
    },
    Stage {
        stage: String,
    },
    Progress {
        stage: Option<String>,
        current: Option<u64>,
        total: Option<u64>,
        overall: Option<f64>,
    },
    Warning {
        message: String,
    },
    Result {
        output_file: String,
        pages: Option<u64>,
    },
    Error {
        code: String,
        message: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SelectOption {
    pub id: String,
    pub label: String,
}

/// Events from `translation_bridge.ts login --events json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoginEvent {
    Select {
        id: String,
        message: String,
        options: Vec<SelectOption>,
    },
    Secret {
        id: String,
        message: String,
    },
    Text {
        id: String,
        message: String,
        placeholder: Option<String>,
    },
    AuthUrl {
        url: String,
        instructions: Option<String>,
    },
    DeviceCode {
        verification_uri: String,
        user_code: String,
    },
    Info {
        message: String,
    },
    Done {
        provider: Option<String>,
        auth_file: Option<String>,
    },
    Error {
        code: Option<String>,
        message: String,
    },
    #[serde(other)]
    Unknown,
}

/// Output of `translation_bridge.ts status --provider X`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct StatusEvent {
    pub installed: bool,
    pub signed_in: Option<bool>,
    pub version: Option<String>,
    pub message: Option<String>,
}

/// Parses one stdout line; non-JSON lines and unknown shapes yield `None`.
pub fn parse_line<T: for<'de> Deserialize<'de>>(line: &str) -> Option<T> {
    serde_json::from_str(line.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_job_events() {
        let event: JobEvent =
            parse_line(r#"{"v":1,"type":"progress","stage":"x","current":1,"total":4,"overall":0.25}"#)
                .unwrap();
        assert_eq!(
            event,
            JobEvent::Progress {
                stage: Some("x".into()),
                current: Some(1),
                total: Some(4),
                overall: Some(0.25)
            }
        );
        let event: JobEvent =
            parse_line(r#"{"v":1,"type":"error","code":"output_exists","message":"nope"}"#).unwrap();
        assert!(matches!(event, JobEvent::Error { ref code, .. } if code == "output_exists"));
        let event: JobEvent = parse_line(r#"{"v":1,"type":"result","output_file":"/a.pdf","pages":3}"#).unwrap();
        assert!(matches!(event, JobEvent::Result { pages: Some(3), .. }));
    }

    #[test]
    fn unknown_event_types_are_tolerated() {
        let event: JobEvent = parse_line(r#"{"v":2,"type":"from_the_future","x":1}"#).unwrap();
        assert_eq!(event, JobEvent::Unknown);
        assert!(parse_line::<JobEvent>("Running: babeldoc ...").is_none());
    }

    #[test]
    fn parses_login_events() {
        let event: LoginEvent = parse_line(
            r#"{"v":1,"type":"select","id":"p1","message":"Pick","options":[{"id":"a","label":"A"}]}"#,
        )
        .unwrap();
        assert!(matches!(event, LoginEvent::Select { ref options, .. } if options.len() == 1));
        let event: LoginEvent =
            parse_line(r#"{"v":1,"type":"device_code","verification_uri":"https://x","user_code":"AB"}"#)
                .unwrap();
        assert!(matches!(event, LoginEvent::DeviceCode { ref user_code, .. } if user_code == "AB"));
        let event: LoginEvent = parse_line(r#"{"v":1,"type":"text","id":"p3","message":"m","placeholder":null}"#).unwrap();
        assert!(matches!(event, LoginEvent::Text { placeholder: None, .. }));
    }

    #[test]
    fn parses_status() {
        let status: StatusEvent = parse_line(
            r#"{"v":1,"type":"status","provider":"codex","installed":true,"signed_in":false,"credential_type":null,"auth_file":"/x"}"#,
        )
        .unwrap();
        assert_eq!(status.signed_in, Some(false));
        let status: StatusEvent =
            parse_line(r#"{"v":1,"type":"status","provider":"claude-code","installed":true,"signed_in":null,"version":"2.1"}"#)
                .unwrap();
        assert_eq!(status.signed_in, None);
    }
}
