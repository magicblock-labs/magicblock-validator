use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

pub fn url_encode(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}
