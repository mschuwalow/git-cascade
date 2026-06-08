use crate::Result;

#[cfg(feature = "test-hooks")]
use crate::Error;

#[cfg(feature = "test-hooks")]
use std::process::Command;

#[cfg(feature = "test-hooks")]
pub fn run(name: &str) -> Result<()> {
    let env_name = format!("GIT_CASCADE_TEST_HOOK_{}", env_key(name));
    let Ok(program) = std::env::var(&env_name) else {
        return Ok(());
    };

    let status = Command::new(program).arg(name).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::TestHookFailed {
            name: name.to_owned(),
            status: status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
        })
    }
}

#[cfg(not(feature = "test-hooks"))]
pub fn run(_name: &str) -> Result<()> {
    Ok(())
}

#[cfg(feature = "test-hooks")]
fn env_key(name: &str) -> String {
    name.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() {
                byte.to_ascii_uppercase() as char
            } else {
                '_'
            }
        })
        .collect()
}
