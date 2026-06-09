use crate::Result;
use crate::apply::{abort as abort_apply, continue_apply};
use crate::git::Git;
use crate::status as status_output;
use crate::storage::Storage;

pub(super) fn continue_operation() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    continue_apply(&git, &storage)?;
    println!("continued cascade operation");

    Ok(())
}

pub(super) fn status() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    print!("{}", status_output::status(&storage)?);

    Ok(())
}

pub(super) fn abort() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    abort_apply(&git, &storage)?;
    println!("aborted cascade operation");

    Ok(())
}
