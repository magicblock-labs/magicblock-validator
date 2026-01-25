//! Shared utility functions

/// URL-encode a string for use in query parameters
///
/// Encodes all characters except unreserved characters as defined in RFC 3986:
/// A-Z, a-z, 0-9, '-', '_', '.', '~'
pub fn url_encode(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len() * 3);
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                encoded.push(c);
            }
            _ => {
                for byte in c.to_string().as_bytes() {
                    encoded.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    encoded
}
