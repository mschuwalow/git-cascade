use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, percent_encode};

use crate::{Error, Result};

const COMPONENT_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'~')
    .add(0x7f);

pub fn encode_component(input: &str) -> String {
    percent_encode(input.as_bytes(), COMPONENT_ENCODE_SET).to_string()
}

pub fn decode_component(input: &str) -> Result<String> {
    validate_encoded_component(input)?;

    percent_decode_str(input)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .map_err(|_| Error::InvalidEncodedComponent {
            component: input.to_owned(),
        })
}

fn validate_encoded_component(input: &str) -> Result<()> {
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let Some(first) = bytes.get(index + 1).and_then(|byte| hex_value(*byte)) else {
                    return invalid_component(input);
                };
                let Some(second) = bytes.get(index + 2).and_then(|byte| hex_value(*byte)) else {
                    return invalid_component(input);
                };
                if is_safe_literal((first << 4) | second) {
                    return invalid_component(input);
                }
                index += 3;
            }
            byte if is_safe_literal(byte) => index += 1,
            _ => return invalid_component(input),
        }
    }

    Ok(())
}

fn invalid_component<T>(input: &str) -> Result<T> {
    Err(Error::InvalidEncodedComponent {
        component: input.to_owned(),
    })
}

fn is_safe_literal(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_component, encode_component};

    #[test]
    fn encodes_branch_names_as_readable_components() {
        let encoded = encode_component("feature/permissions stack");

        assert_eq!(encoded, "feature%2Fpermissions%20stack");
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains(' '));
    }

    #[test]
    fn round_trips_unicode_branch_names() {
        let original = "topic/uber-cafe-☕";

        assert_eq!(
            decode_component(&encode_component(original)).unwrap(),
            original
        );
    }

    #[test]
    fn rejects_invalid_components() {
        assert!(decode_component("not valid!").is_err());
    }

    #[test]
    fn accepts_lowercase_hex_escapes() {
        assert_eq!(decode_component("feature%2ffoo").unwrap(), "feature/foo");
    }

    #[test]
    fn rejects_non_canonical_escaped_safe_literals() {
        assert!(decode_component("feature%2Dfoo").is_err());
    }

    #[test]
    fn rejects_malformed_percent_escapes() {
        assert!(decode_component("feature%").is_err());
        assert!(decode_component("feature%2").is_err());
        assert!(decode_component("feature%xx").is_err());
    }
}
