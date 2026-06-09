use crate::git::Git;
use crate::{Error, Result};

pub(super) struct Inference {
    pub(super) old_base: String,
    pub(super) new_tip: String,
}

pub(super) fn infer_range(
    git: &Git,
    old_tip: &str,
    onto: &str,
    old_base: Option<String>,
) -> Result<Inference> {
    if let Some(old_base) = old_base {
        return Ok(Inference {
            old_base,
            new_tip: onto.to_owned(),
        });
    }

    let old_tip_commit = git.resolve_commit(old_tip)?;
    let onto_commit = git.resolve_commit(onto)?;

    if !git.is_ancestor(&old_tip_commit, &onto_commit)? {
        return Ok(Inference {
            old_base: onto.to_owned(),
            new_tip: onto.to_owned(),
        });
    }

    if let Some(landing) = find_landing_merge(git, &old_tip_commit, &onto_commit)? {
        return Ok(Inference {
            old_base: landing.first_parent,
            new_tip: landing.commit,
        });
    }

    Err(Error::InvalidInvocation(format!(
        "cannot infer old base for landed branch `{old_tip}`; it is already contained in `{onto}`, but no first-parent merge commit landing it was found. This can happen after a fast-forward merge. Pass --old-base <previous-main-tip>."
    )))
}

struct LandingMerge {
    commit: String,
    first_parent: String,
}

fn find_landing_merge(git: &Git, old_tip: &str, onto: &str) -> Result<Option<LandingMerge>> {
    for commit in git.rev_list_first_parent_merges(onto)? {
        let parents = git.commit_parents(&commit)?;
        let Some(first_parent) = parents.first() else {
            continue;
        };
        if git.is_ancestor(old_tip, first_parent)? {
            continue;
        }

        let mut matching_parents = Vec::new();
        for parent in parents.iter().skip(1) {
            if git.is_ancestor(old_tip, parent)? {
                matching_parents.push(parent);
            }
        }

        if matching_parents.len() > 1 {
            return Err(Error::InvalidInvocation(format!(
                "cannot infer landed merge commit `{commit}` because multiple non-first parents contain the old tip; pass --old-base <ref>"
            )));
        }

        if matching_parents.len() == 1 {
            return Ok(Some(LandingMerge {
                commit,
                first_parent: first_parent.clone(),
            }));
        }
    }

    Ok(None)
}
