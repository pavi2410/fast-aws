//! IAM returns policy documents URL-encoded; this turns them back into JSON.

use serde_json::Value;

/// Decodes `%XX` escapes (and `+` as a space). Invalid escapes are kept as-is.
pub(crate) fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A policy document as JSON. Falls back to the decoded text if it isn't JSON.
pub(crate) fn decode_document(encoded: &str) -> Value {
    let text = url_decode(encoded);
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(url_decode("%7B%22a%22%3A1%7D"), r#"{"a":1}"#);
        assert_eq!(url_decode("a+b%20c"), "a b c");
        assert_eq!(url_decode("100%"), "100%");
        assert_eq!(url_decode("%zz"), "%zz");
        assert_eq!(url_decode("%C3%A9"), "é");
    }

    #[test]
    fn documents_become_json() {
        assert_eq!(decode_document("%7B%22a%22%3A1%7D")["a"], 1);
        assert_eq!(
            decode_document("not%20json"),
            Value::String("not json".into())
        );
    }
}
