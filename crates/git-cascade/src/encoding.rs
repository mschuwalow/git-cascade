use crate::{Error, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub fn encode_component(input: &str) -> String {
    URL_SAFE_NO_PAD.encode(input.as_bytes())
}

pub fn decode_component(input: &str) -> Result<String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(input)
        .map_err(|_| Error::InvalidEncodedComponent {
            component: input.to_owned(),
        })?;

    String::from_utf8(bytes).map_err(|_| Error::InvalidEncodedComponent {
        component: input.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::{decode_component, encode_component};

    #[test]
    fn encodes_branch_names_as_url_safe_components() {
        let encoded = encode_component("feature/permissions stack");

        assert_eq!(encoded, "ZmVhdHVyZS9wZXJtaXNzaW9ucyBzdGFjaw");
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('='));
    }

    #[test]
    fn round_trips_unicode_branch_names() {
        let original = "topic/uber-cafe";

        assert_eq!(
            decode_component(&encode_component(original)).unwrap(),
            original
        );
    }

    #[test]
    fn rejects_invalid_components() {
        assert!(decode_component("not valid!").is_err());
    }
}
