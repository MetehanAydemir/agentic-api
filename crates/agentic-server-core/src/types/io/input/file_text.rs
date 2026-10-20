//! Inline UTF-8 text files carried by message `input_file` parts.
//!
//! A message file is accepted only when its `file_data` is a base64 text file:
//! a data URL with a text media type, or raw base64 (or an
//! `application/octet-stream` data URL) whose `filename` has a known text
//! extension. The client's `input_file` is what the gateway stores; only the
//! model-facing copy replaces it with an `input_text` part.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use thiserror::Error;

use super::{InputContent, InputFileContent, InputItem, InputMessageContent, ResponsesInput};
use crate::types::io::InOutItem;

/// Non-`text/*` media types whose content is text.
const TEXT_MEDIA_TYPES: [&str; 4] = [
    "application/json",
    "application/xml",
    "application/yaml",
    "application/x-yaml",
];

/// File extensions accepted when the data carries no specific media type.
const TEXT_EXTENSIONS: [&str; 35] = [
    "c", "cc", "cfg", "conf", "cpp", "css", "csv", "go", "h", "hpp", "htm", "html", "ini", "java", "js", "json",
    "jsonl", "log", "markdown", "md", "mjs", "ndjson", "py", "rs", "rst", "sh", "sql", "tex", "toml", "ts", "tsv",
    "txt", "xml", "yaml", "yml",
];

/// Why a message `input_file` cannot be inlined as text.
///
/// Messages never echo the file data, URL, name, or media type.
#[derive(Debug, Error, PartialEq, Eq)]
pub(super) enum InputFileError {
    #[error(
        "input_file with file_id is not supported: this gateway has no Files API; send the file inline as \
         file_data (a base64 UTF-8 text file) or as input_text"
    )]
    FileId,
    #[error(
        "input_file with file_url is not supported: the gateway does not fetch files; send the file inline as \
         file_data (a base64 UTF-8 text file) or as input_text"
    )]
    FileUrl,
    #[error("input_file without file_data is not supported; send the file inline as file_data")]
    MissingData,
    #[error(
        "input_file content is not supported: only UTF-8 text files (text/*, application/json, application/xml, \
         application/yaml) can be inlined; extract the text client-side and send it as input_text, or send pages \
         as input_image"
    )]
    NotText,
    #[error("input_file file_data is not supported: data URLs must be base64-encoded (data:<media type>;base64,...)")]
    NotBase64DataUrl,
    #[error("input_file file_data is not supported: it is not valid base64")]
    InvalidBase64,
    #[error("input_file file_data is not supported: the decoded file is not valid UTF-8 text")]
    NotUtf8,
}

fn is_text_media_type(media_type: &str) -> bool {
    let media_type = media_type.trim();
    media_type
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/"))
        || TEXT_MEDIA_TYPES
            .iter()
            .any(|known| known.eq_ignore_ascii_case(media_type))
}

fn has_text_extension(filename: Option<&str>) -> bool {
    filename
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, extension)| {
            TEXT_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Split a base64 data URL into its media type and payload.
fn parse_data_url(url: &str) -> Result<(&str, &str), InputFileError> {
    let (header, payload) = url.split_once(',').ok_or(InputFileError::NotBase64DataUrl)?;
    // `data:<media type>[;<parameter>]*;base64`: parameters such as `charset` are ignored.
    let (media_type, encoding) = header.rsplit_once(';').ok_or(InputFileError::NotBase64DataUrl)?;
    if !encoding.eq_ignore_ascii_case("base64") {
        return Err(InputFileError::NotBase64DataUrl);
    }
    let media_type = media_type
        .split_once(';')
        .map_or(media_type, |(media_type, _)| media_type);
    Ok((media_type.trim(), payload))
}

impl InputFileContent {
    /// Check the reference fields and media type without decoding, returning the base64 payload.
    fn text_payload(&self) -> Result<&str, InputFileError> {
        if self.file_id.is_some() {
            return Err(InputFileError::FileId);
        }
        if self.file_url.is_some() {
            return Err(InputFileError::FileUrl);
        }
        let data = self.file_data.as_deref().ok_or(InputFileError::MissingData)?;
        let filename = self.filename.as_deref();
        let (media_type, payload) = match data.get(..5) {
            Some(scheme) if scheme.eq_ignore_ascii_case("data:") => parse_data_url(&data[5..])?,
            _ => ("", data),
        };
        // An explicit media type wins; a generic one falls back to the filename.
        let is_text = if media_type.is_empty() || media_type.eq_ignore_ascii_case("application/octet-stream") {
            has_text_extension(filename)
        } else {
            is_text_media_type(media_type)
        };
        if is_text {
            Ok(payload)
        } else {
            Err(InputFileError::NotText)
        }
    }

    /// Decode the file as UTF-8 text, prefixed with `File: <filename>` when a filename is set.
    ///
    /// # Errors
    /// Returns why the file cannot be inlined: a file reference, missing data,
    /// a non-text media type, or data that is not base64-encoded UTF-8.
    pub(super) fn inline_text(&self) -> Result<String, InputFileError> {
        let bytes = STANDARD
            .decode(self.text_payload()?)
            .map_err(|_| InputFileError::InvalidBase64)?;
        let text = String::from_utf8(bytes).map_err(|_| InputFileError::NotUtf8)?;
        Ok(match self.filename.as_deref() {
            Some(filename) => format!("File: {filename}\n{text}"),
            None => text,
        })
    }
}

impl ResponsesInput {
    /// Whether a message carries an `input_file` the gateway inlines as text.
    ///
    /// Classifies the reference fields and media type only, without decoding,
    /// so routing can send such requests to the executor even with
    /// `store: false`. Malformed data is rejected later by [`Self::validate`].
    #[must_use]
    pub fn has_inline_text_file(&self) -> bool {
        matches!(self, Self::Items(items) if items.iter().any(|item| matches!(item,
            InOutItem::Input(InputItem::Message(message)) if matches!(&message.content,
                InputMessageContent::Parts(parts) if parts.iter().any(|part| matches!(part,
                    InputContent::InputFile(file) if file.text_payload().is_ok()))))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn file(fields: serde_json::Value) -> InputFileContent {
        serde_json::from_value(fields).unwrap()
    }

    fn data(data: &str, filename: Option<&str>) -> InputFileContent {
        InputFileContent {
            file_data: Some(data.to_owned()),
            filename: filename.map(str::to_owned),
            ..InputFileContent::default()
        }
    }

    fn encode(text: &[u8]) -> String {
        STANDARD.encode(text)
    }

    #[test]
    fn accepts_text_media_types_with_and_without_filename() {
        for media_type in [
            "text/plain",
            "text/markdown",
            "TEXT/CSV",
            "application/json",
            "application/xml",
        ] {
            let url = format!("data:{media_type};base64,{}", encode(b"a: 1"));
            assert_eq!(data(&url, None).inline_text().unwrap(), "a: 1", "{media_type}");
        }
        let url = format!("data:application/yaml;base64,{}", encode(b"a: 1"));
        assert_eq!(
            data(&url, Some("config.yaml")).inline_text().unwrap(),
            "File: config.yaml\na: 1"
        );
    }

    #[test]
    fn ignores_media_type_parameters() {
        let url = format!("data:text/plain;charset=utf-8;base64,{}", encode("héllo".as_bytes()));
        assert_eq!(
            data(&url, Some("notes.txt")).inline_text().unwrap(),
            "File: notes.txt\nhéllo"
        );
    }

    #[test]
    fn generic_or_missing_media_type_falls_back_to_the_filename() {
        let payload = encode(b"# Notes");
        let octet = format!("data:application/octet-stream;base64,{payload}");
        assert_eq!(
            data(&octet, Some("notes.md")).inline_text().unwrap(),
            "File: notes.md\n# Notes"
        );
        assert_eq!(
            data(&payload, Some("config.YML")).inline_text().unwrap(),
            "File: config.YML\n# Notes"
        );
        assert_eq!(
            data(&payload, Some("config.yaml")).inline_text().unwrap(),
            "File: config.yaml\n# Notes"
        );
        assert_eq!(data(&payload, None).inline_text(), Err(InputFileError::NotText));
        assert_eq!(
            data(&octet, Some("archive.zip")).inline_text(),
            Err(InputFileError::NotText)
        );
        let empty_type = format!("data:;base64,{payload}");
        assert_eq!(
            data(&empty_type, Some("a.txt")).inline_text().unwrap(),
            "File: a.txt\n# Notes"
        );
    }

    #[test]
    fn rejects_pdf_by_media_type_or_filename() {
        let pdf = format!("data:application/pdf;base64,{}", encode(b"%PDF-1.7"));
        // An explicit media type wins over a text filename.
        assert_eq!(data(&pdf, Some("a.txt")).inline_text(), Err(InputFileError::NotText));
        assert_eq!(
            data(&encode(b"%PDF-1.7"), Some("report.pdf")).inline_text(),
            Err(InputFileError::NotText)
        );
    }

    #[test]
    fn rejects_references_and_missing_data() {
        let reference = file(json!({"file_id":"file_1", "file_data":"aGk=", "filename":"a.txt"}));
        assert_eq!(reference.inline_text(), Err(InputFileError::FileId));
        let url = file(json!({"file_url":"https://example.invalid/a.txt"}));
        assert_eq!(url.inline_text(), Err(InputFileError::FileUrl));
        assert_eq!(
            file(json!({"filename":"a.txt"})).inline_text(),
            Err(InputFileError::MissingData)
        );
    }

    #[test]
    fn rejects_undecodable_data() {
        assert_eq!(
            data("data:text/plain,hello", None).inline_text(),
            Err(InputFileError::NotBase64DataUrl)
        );
        assert_eq!(
            data("data:text/plain", None).inline_text(),
            Err(InputFileError::NotBase64DataUrl)
        );
        assert_eq!(
            data("data:text/plain;base64,not base64!", None).inline_text(),
            Err(InputFileError::InvalidBase64)
        );
        let latin1 = format!("data:text/plain;base64,{}", encode(&[0x68, 0xe9, 0x6c]));
        assert_eq!(data(&latin1, None).inline_text(), Err(InputFileError::NotUtf8));
    }

    #[test]
    fn accepts_empty_and_multibyte_text() {
        assert_eq!(
            data("data:text/plain;base64,", Some("empty.txt"))
                .inline_text()
                .unwrap(),
            "File: empty.txt\n"
        );
        let url = format!("data:text/plain;base64,{}", encode("çğış 日本 🙂".as_bytes()));
        assert_eq!(data(&url, None).inline_text().unwrap(), "çğış 日本 🙂");
    }

    #[test]
    fn routing_classifies_without_decoding() {
        let input = |part: serde_json::Value| -> ResponsesInput {
            serde_json::from_value(json!([{"role":"user", "content":[{"type":"input_text", "text":"hi"}, part]}]))
                .unwrap()
        };
        // Classification does not decode, so malformed text data still routes to the executor.
        assert!(input(json!({"type":"input_file", "file_data":"data:text/plain;base64,!!"})).has_inline_text_file());
        for part in [
            json!({"type":"input_file", "file_id":"file_1"}),
            json!({"type":"input_file", "file_url":"https://example.invalid/a.txt"}),
            json!({"type":"input_file", "file_data":"data:application/pdf;base64,JVBERi0="}),
            json!({"type":"input_image", "image_url":"https://example.invalid/a.png"}),
        ] {
            assert!(!input(part.clone()).has_inline_text_file(), "{part}");
        }
        assert!(!ResponsesInput::Text("hi".to_owned()).has_inline_text_file());
        let tool_output: ResponsesInput = serde_json::from_value(json!([
            {"type":"function_call_output", "call_id":"call_1", "output":[
                {"type":"input_file", "file_data":"data:text/plain;base64,aGk="}]}
        ]))
        .unwrap();
        assert!(!tool_output.has_inline_text_file());
    }
}
