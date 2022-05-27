use std::ffi::OsString;

use anyhow::anyhow;

use librad::git::storage::ReadOnly;
use librad::git::Storage;
use librad::profile::Profile;

use radicle_common::args::{Args, Error, Help};
use radicle_common::{cobs, git, keys, patch, person, profile, project};
use radicle_terminal as term;

pub const HELP: Help = Help {
    name: "patch",
    description: env!("CARGO_PKG_DESCRIPTION"),
    version: env!("CARGO_PKG_VERSION"),
    usage: r#"
Usage

    rad patch [<option>...]
    rad patch [--no-sync]

Create options

    --[no-]sync       Sync patch to seed (default: sync)

Options

    --list            List all patches (default: false)
    --help            Print help
"#,
};

#[derive(Default, Debug)]
pub struct Options {
    pub list: bool,
    pub verbose: bool,
    pub sync: bool,
}

impl Args for Options {
    fn from_args(args: Vec<OsString>) -> anyhow::Result<(Self, Vec<OsString>)> {
        use lexopt::prelude::*;

        let mut parser = lexopt::Parser::from_args(args);
        let mut list = false;
        let mut verbose = false;
        let mut sync = true;

        if let Some(arg) = parser.next()? {
            match arg {
                Long("list") | Short('l') => {
                    list = true;
                }
                Long("verbose") | Short('v') => {
                    verbose = true;
                }
                Long("sync") => {
                    sync = true;
                }
                Long("no-sync") => {
                    sync = false;
                }
                Long("help") => {
                    return Err(Error::Help.into());
                }
                _ => return Err(anyhow::anyhow!(arg.unexpected())),
            }
        }

        Ok((
            Options {
                list,
                sync,
                verbose,
            },
            vec![],
        ))
    }
}

pub fn run(options: Options) -> anyhow::Result<()> {
    let (urn, repo) = project::cwd()
        .map_err(|_| anyhow!("this command must be run in the context of a project"))?;

    let profile = profile::default()?;
    let signer = term::signer(&profile)?;
    let storage = keys::storage(&profile, signer)?;
    let project = project::get(&storage, &urn)?
        .ok_or_else(|| anyhow!("couldn't load project {} from local state", urn))?;

    if options.list {
        list(&storage, &project, &repo)?;
    } else {
        create(&storage, &profile, &project, &repo, &options)?;
    }

    Ok(())
}

fn list(
    storage: &Storage,
    project: &project::Metadata,
    repo: &git::Repository,
) -> anyhow::Result<()> {
    term::headline(&format!(
        "🌱 Listing patches for {}.",
        term::format::highlight(&project.name)
    ));

    let mut table = term::Table::default();
    let blank = ["".to_owned(), "".to_owned()];

    table.push([
        format!("[{}]", term::format::secondary("Open")),
        String::new(),
    ]);
    table.push(blank.clone());
    list_by_state(storage, repo, project, &mut table, patch::State::Open)?;
    table.push(blank.clone());
    table.push(blank.clone());

    table.push([
        format!("[{}]", term::format::positive("Merged")),
        String::new(),
    ]);
    table.push(blank);
    list_by_state(storage, repo, project, &mut table, patch::State::Merged)?;
    table.render();

    term::blank();

    Ok(())
}

fn create(
    storage: &Storage,
    profile: &Profile,
    project: &project::Metadata,
    repo: &git::Repository,
    options: &Options,
) -> anyhow::Result<()> {
    let head = repo.head()?;
    let current_branch = head.shorthand().unwrap_or("HEAD (no branch)");

    term::headline(&format!(
        "🌱 Creating patch for {}.",
        term::format::highlight(&project.name)
    ));

    let target = repo
        .resolve_reference_from_short_name(&format!("rad/{}", &project.default_branch))?
        .target();
    let target_oid = target
        .map(|h| format!("{:.7}", h.to_string()))
        .unwrap_or_else(String::new);

    let head_ref = head.target();
    let head_oid = head_ref
        .map(|h| format!("{:.7}", h.to_string()))
        .unwrap_or_else(String::new);

    term::info!(
        "{} ({}) <- {} ({})",
        term::format::highlight(&project.default_branch.clone()),
        term::format::secondary(&target_oid),
        term::format::highlight(&current_branch),
        term::format::secondary(&head_oid),
    );

    let (ahead, behind) = repo.graph_ahead_behind(
        head_ref.unwrap_or_else(git::Oid::zero),
        target.unwrap_or_else(git::Oid::zero),
    )?;
    term::info!(
        "This branch is {} commit(s) ahead, {} commit(s) behind {}.",
        term::format::highlight(ahead),
        term::format::highlight(behind),
        term::format::highlight(&project.default_branch)
    );

    let merge_base_ref = repo.merge_base(
        target.unwrap_or_else(git::Oid::zero),
        head_ref.unwrap_or_else(git::Oid::zero),
    );

    term::patch::list_commits(repo, &merge_base_ref.unwrap(), &head_ref.unwrap(), true)?;
    term::blank();

    let title: String = term::text_input("Title", None)?;
    let description = match term::Editor::new().edit("").unwrap() {
        Some(rv) => rv,
        None => String::new(),
    };
    term::success!(
        "{} {}",
        term::format::tertiary_bold("Description".to_string()),
        term::format::tertiary("·".to_string()),
    );
    term::markdown(&description);
    term::blank();

    if term::confirm("Propose patch?") {
        let message = [title.clone(), description.clone()].join("\n");
        let tag = create_patch(repo, &message, options.verbose)?;

        let whoami = person::local(storage)?;
        let patches = cobs::patch::Patches::new(whoami, profile.paths(), storage)?;
        let target = &project.default_branch;
        let id = patches.create(&project.urn, &title, &description, target, tag, &[])?;

        term::info!("Patch {} created", id);

        if options.sync {
            sync(current_branch.to_owned())?;
        }
    } else {
        return Err(anyhow!("Canceled."));
    }

    term::blank();
    term::success!(
        "🌱 Proposed patch {}",
        term::format::highlight(&current_branch)
    );

    Ok(())
}

fn list_by_state(
    storage: &Storage,
    repo: &git::Repository,
    project: &project::Metadata,
    table: &mut term::Table<2>,
    state: patch::State,
) -> anyhow::Result<()> {
    let mut patches: Vec<patch::Tag> = patch::all(project, None, &storage)?;

    for (_, info) in project::tracked(project, storage)? {
        let mut theirs = patch::all(project, Some(info), &storage)?;
        patches.append(&mut theirs);
    }
    patches.retain(|patch| state == patch::state(repo, patch));

    if !patches.is_empty() {
        for patch in patches {
            print(storage, &patch, table)?;
        }
    } else {
        table.push(["No patches found.".to_owned(), String::new()]);
    }

    Ok(())
}

/// Create and push tag to monorepo.
pub fn create_patch(
    repo: &git::Repository,
    message: &str,
    verbose: bool,
) -> anyhow::Result<git::Oid> {
    let head = repo.head()?;
    let current_branch = head.shorthand().unwrap_or("HEAD (no branch)");
    let patch_tag_name = format!("{}{}", patch::TAG_PREFIX, &current_branch);

    let mut spinner = term::spinner("Adding tag...");
    let tag = match git::add_tag(repo, message, &patch_tag_name) {
        Ok(tag) => tag,
        Err(err) => {
            spinner.failed();
            return Err(err);
        }
    };

    spinner.message("Pushing tag...".to_owned());
    match git::push_tag(&patch_tag_name) {
        Ok(output) => {
            if verbose {
                term::blob(output);
            }
        }
        Err(err) => {
            spinner.failed();
            return Err(err);
        }
    };

    spinner.message("Pushing branch...".to_owned());
    match git::push_branch(current_branch) {
        Ok(output) => {
            if verbose {
                term::blob(output);
            }
        }
        Err(err) => {
            spinner.failed();
            return Err(err);
        }
    };

    spinner.finish();

    Ok(tag)
}

/// Adds patch details as a new row to `table` and render later.
pub fn print<S>(storage: &S, patch: &patch::Tag, table: &mut term::Table<2>) -> anyhow::Result<()>
where
    S: AsRef<ReadOnly>,
{
    let storage = storage.as_ref();

    if let Some(message) = patch.message.clone() {
        let you = patch.peer.id == *storage.peer_id();
        let title = message.lines().next().unwrap_or("");
        let name = term::format::tertiary(&patch.id);

        let mut author_info = vec![term::format::italic(format!(
            "└── Opened by {}",
            &patch.peer.name()
        ))];

        if you {
            author_info.push(term::format::badge_secondary("you"));
        }

        table.push([term::format::bold(title), "".to_owned()]);
        table.push([author_info.join(" "), name]);
    }
    Ok(())
}

pub fn sync(current_branch: String) -> anyhow::Result<()> {
    let sync_options = rad_sync::Options {
        refs: rad_sync::Refs::Branch(current_branch),
        verbose: false,
        ..rad_sync::Options::default()
    };
    rad_sync::run(sync_options)?;

    Ok(())
}
