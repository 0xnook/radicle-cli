use std::collections::{BTreeSet, HashSet};
use std::convert::{TryFrom, TryInto};
use std::path::Path;

use anyhow::{anyhow, Context as _, Error, Result};
use either::Either;
use git2::Repository;
use git_repository as git;

use librad::crypto::BoxedSigner;
use librad::git::identities::{self, Project};
use librad::git::local::url::LocalUrl;
use librad::git::storage::Storage;
use librad::git::types::remote::Remote;
use librad::git::Urn;
use librad::identities::payload;
use librad::identities::SomeIdentity;
use librad::profile::Profile;
use librad::reflike;
use librad::PeerId;

use rad_identities::{self, project};

/// Project metadata.
#[derive(Debug)]
pub struct Metadata {
    /// Project name.
    pub name: String,
    /// Project description.
    pub description: String,
    /// Default branch of project.
    pub default_branch: String,
    /// List of delegates.
    pub delegates: HashSet<Urn>,
    /// List of remotes.
    pub remotes: HashSet<PeerId>,
}

impl TryFrom<librad::identities::Project> for Metadata {
    type Error = anyhow::Error;

    fn try_from(project: librad::identities::Project) -> Result<Self, Self::Error> {
        let subject = project.subject();
        let delegates = project
            .delegations()
            .iter()
            .indirect()
            .map(|indirect| indirect.urn())
            .collect();
        let remotes = project
            .delegations()
            .iter()
            .flat_map(|either| match either {
                Either::Left(pk) => Either::Left(std::iter::once(PeerId::from(*pk))),
                Either::Right(indirect) => {
                    Either::Right(indirect.delegations().iter().map(|pk| PeerId::from(*pk)))
                }
            })
            .collect::<HashSet<PeerId>>();
        let default_branch = subject
            .default_branch
            .clone()
            .ok_or(anyhow!("project is missing a default branch"))?
            .to_string();

        Ok(Self {
            name: subject.name.to_string(),
            description: subject
                .description
                .clone()
                .map_or_else(|| "".into(), |desc| desc.to_string()),
            default_branch,
            delegates,
            remotes,
        })
    }
}

pub fn create(
    storage: &Storage,
    signer: BoxedSigner,
    profile: &Profile,
    payload: payload::Project,
) -> Result<Project, Error> {
    // Currently, radicle link adds the project name to the path, so we're forced to
    // have them match, and specify the parent folder instead of the current folder.
    let path = Path::new("..").to_path_buf();
    let paths = profile.paths().clone();
    let whoami = project::WhoAmI::from(None);
    let delegations = BTreeSet::new();

    project::create::<payload::Project>(
        storage,
        paths,
        signer,
        whoami,
        delegations,
        payload,
        vec![],
        rad_identities::project::Creation::Existing { path },
    )
}

pub fn list(storage: &Storage) -> Result<Vec<(Urn, Metadata, Option<git::ObjectId>)>, Error> {
    let repo = git::Repository::open(storage.path())?;
    let objs = identities::any::list(storage)?
        .filter_map(|res| {
            res.map(|id| match id {
                SomeIdentity::Project(project) => {
                    let urn = project.urn();
                    let meta: Metadata = project.try_into().ok()?;
                    let head = get_local_head(&repo, &urn, &meta.default_branch)
                        .ok()
                        .flatten();

                    Some((urn, meta, head))
                }
                _ => None,
            })
            .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(objs)
}

pub fn get_local_head<'r>(
    repo: &'r git::Repository,
    urn: &Urn,
    branch: &str,
) -> Result<Option<git::ObjectId>, Error> {
    let mut repo = repo.to_easy();
    repo.set_namespace(urn.encode_id())?;

    let reference = repo.try_find_reference(format!("heads/{}", branch))?;

    Ok(reference.map(|r| r.id().detach()))
}

pub fn get(storage: &Storage, urn: &Urn) -> Result<Option<Metadata>, Error> {
    let proj = project::get(storage, urn)?;
    let meta = proj.map(|p| p.try_into()).transpose()?;

    Ok(meta)
}

pub fn repository() -> Result<Repository, Error> {
    match Repository::open(".") {
        Ok(repo) => Ok(repo),
        Err(err) => Err(err).context("the current working directory is not a git repository"),
    }
}

pub fn remote(repo: &Repository) -> Result<Remote<LocalUrl>, Error> {
    match Remote::<LocalUrl>::find(repo, reflike!("rad")) {
        Ok(Some(remote)) => Ok(remote),
        Ok(None) => Err(anyhow!(
            "could not find radicle remote in git config. Did you forget to run `rad init`?"
        )),
        Err(err) => Err(err).context("could not read git remote configuration"),
    }
}

pub fn urn() -> Result<Urn, Error> {
    let repo = self::repository()?;
    Ok(self::remote(&repo)?.url.urn)
}
