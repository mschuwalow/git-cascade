use crate::{Error, Result};

pub fn validate(name: &str) -> Result<()> {
    if name.is_empty() {
        return invalid(name, "must not be empty");
    }

    if name == "." || name == ".." {
        return invalid(name, "must be a normal path component");
    }

    if name.contains("..") {
        return invalid(name, "must not contain `..`");
    }

    if name.ends_with(".lock") {
        return invalid(name, "must not end with `.lock`");
    }

    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return invalid(
            name,
            "may only contain ASCII letters, digits, `.`, `_`, and `-`",
        );
    }

    Ok(())
}

fn invalid<T>(name: &str, reason: impl Into<String>) -> Result<T> {
    Err(Error::InvalidPlanName {
        name: name.to_owned(),
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn accepts_safe_plan_names() {
        for name in ["stack", "permissions-stack", "v1.2_stack", "a"] {
            validate(name).unwrap();
        }
    }

    #[test]
    fn rejects_unsafe_plan_names() {
        for name in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "has space",
            "nested..dots",
            "state.lock",
            "feature@{1}",
        ] {
            assert!(validate(name).is_err(), "{name} should be invalid");
        }
    }
}
