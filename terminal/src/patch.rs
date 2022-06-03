use radicle_common::git;

use crate as term;

/// List all commits between `left` and `right` in the given repository.
pub fn list_commits(
    repo: &git::Repository,
    left: &git::Oid,
    right: &git::Oid,
) -> anyhow::Result<()> {
    let mut table = term::Table::default();

    let left_short = format!("{:.7}", left.to_string());
    let right_short = format!("{:.7}", right.to_string());

    let mut revwalk = repo.revwalk()?;
    revwalk.push_range(&format!("{}..{}", left_short, right_short))?;

    let mut revwalk = repo.revwalk()?;
    revwalk.push_range(&format!("{}..{}", left_short, right_short))?;

    for rev in revwalk {
        let commit = repo.find_commit(rev?)?;
        let message = commit
            .summary_bytes()
            .unwrap_or_else(|| commit.message_bytes());
        table.push([
            term::format::secondary(format!("{:.7}", commit.id().to_string())),
            term::format::italic(String::from_utf8_lossy(message)),
        ]);
    }
    table.render();

    Ok(())
}
