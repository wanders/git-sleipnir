use std::collections::HashMap;
use std::collections::HashSet;
use std::error::Error;
use std::io::Write;
use std::path::Path;

use clap::{Args, Parser, Subcommand};
use url::Url;

use log::{debug, info};

mod branch_fallback;
mod git_http_client;
mod local_repo;
mod pkt_line;
mod reader;
mod util;

use crate::branch_fallback::BranchFallback;
use crate::git_http_client::GitClient;
use crate::local_repo::LocalRepo;

#[derive(Debug)]
pub enum ShallowInfo {
    Shallow(String),
    NotShallow(String),
}

#[derive(Debug)]
struct RefInfo {
    sha: String,
    refname: String,
    peeled: Option<String>,
}

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Clone(CloneArgs),
    FindBranch(FindBranchArgs),
}

#[derive(Args)]
struct CloneArgs {
    #[arg(long)]
    base_url: Option<Url>,

    #[arg(long)]
    branches_starting_with: Option<String>,

    #[arg(long)]
    tags_starting_with: Option<String>,

    #[arg(long)]
    branch: String,

    #[arg(long = "branch-fallback", action = clap::ArgAction::Append, value_parser = BranchFallback::parse)]
    fallbacks: Vec<BranchFallback>,

    #[arg(long)]
    default_branch: Option<String>,

    #[arg(long)]
    tag_output_file: Option<String>,

    #[arg(required = true)]
    urls: Vec<String>,
}

#[derive(Args)]
struct FindBranchArgs {
    #[arg(long)]
    branches_starting_with: Option<String>,

    #[arg(long)]
    branch: String,

    #[arg(long = "branch-fallback", action = clap::ArgAction::Append, value_parser = BranchFallback::parse)]
    fallbacks: Vec<BranchFallback>,

    #[arg(long)]
    default_branch: Option<String>,

    #[arg(required = true)]
    repo_url: String,
}

fn resolve_urls(base: Option<&Url>, urls: &[String]) -> Result<Vec<Url>, String> {
    urls.iter()
        .map(|url_str| match (base, Url::parse(url_str)) {
            (_, Ok(url)) => Ok(url),
            (Some(base), Err(_)) => base.join(url_str).map_err(|e| e.to_string()),
            (None, Err(_)) => Err(format!("Relative URL '{}' requires --base-url", url_str)),
        })
        .collect()
}

fn masked_url(orig: &Url) -> String {
    let mut url = orig.clone();

    if url.password().is_some() {
        let _ = url.set_password(Some("XXXXXXXX"));
    }

    url.to_string()
}

async fn clone_one(url: &Url, opts: &CloneArgs) -> Result<String, Box<dyn Error>> {
    let client = GitClient::new();

    let remote_repo = client.for_url(url);

    let mut local_repo_path = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .expect("Not a proper path");
    if let Some(stripped) = local_repo_path.strip_suffix(".git") {
        local_repo_path = stripped;
    }
    info!("Creating local repo {}", local_repo_path);

    let local_repo = LocalRepo::init_new(Path::new(local_repo_path)).await?;

    let mut wanted_refs = Vec::new();
    match &opts.branches_starting_with {
        Some(branches_starting_with) => {
            wanted_refs.push(format!("refs/heads/{}", branches_starting_with))
        }
        None => wanted_refs.push("refs/heads/".to_string()),
    }
    match &opts.tags_starting_with {
        Some(tags_starting_with) => wanted_refs.push(format!("refs/tags/{}", tags_starting_with)),
        None => wanted_refs.push("refs/tags/".to_string()),
    }

    debug!("Listing remote refs (wanted refs: {:?})", wanted_refs);
    let refs = remote_repo.ls_refs(&wanted_refs).await?;

    let mut tagged_commits = HashSet::new();
    let mut available_branches = HashMap::<&str, &RefInfo>::new();
    for r in &refs {
        if let Some(sha) = &r.peeled {
            tagged_commits.insert(sha);
        }

        if let Some(branchname) = r.refname.strip_prefix("refs/heads/") {
            available_branches.insert(branchname, r);
        }
    }

    let mut branch: Option<&RefInfo> =
        branch_fallback::resolve(&opts.branch, &opts.fallbacks, &available_branches);
    debug!("Found branch: {:?}", branch);
    if branch.is_none() && opts.default_branch.is_some() {
        branch = available_branches
            .get(opts.default_branch.as_ref().unwrap().as_str())
            .map(|v| &**v);
    }
    if branch.is_none() {
        panic!("No suitable branch found");
    }

    let branch = branch.unwrap();
    debug!("Using branch: {} (sha: {})", branch.refname, branch.sha);

    info!("Getting: {}", branch.refname);

    let mut depth = 1;
    let mut commits;
    loop {
        remote_repo
            .shallow_fetch(&local_repo, &branch.sha, depth)
            .await?;

        local_repo.update_ref(&branch.refname, &branch.sha).await?;
        local_repo.update_head(&branch.refname).await?;

        commits = local_repo.rev_list(&branch.sha).await?;
        if commits.iter().any(|sha| tagged_commits.contains(sha)) {
            break;
        }

        depth += 50;
        info!("Could not find tag in shallow clone. Deepening... (depth={depth})");
    }

    let interesting_commits: HashSet<&str> = commits.iter().map(|s| s.as_str()).collect();
    let mut reachable_tags = Vec::new();
    for r in &refs {
        if let (Some(sha), Some(tagname)) = (&r.peeled, r.refname.strip_prefix("refs/tags/")) {
            if interesting_commits.contains(sha.as_str()) {
                reachable_tags.push(tagname);
                local_repo.update_ref(&r.refname, &r.sha).await?;
            }
        }
    }

    local_repo.checkout_head().await?;

    let maxtag = reachable_tags
        .iter()
        .max_by(|a, b| natord::compare(a, b))
        .map(|t| t.to_string())
        .unwrap();

    Ok(maxtag)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .init();

    let opts = Cli::parse();

    match opts.command {
        Command::Clone(args) => main_clone(args).await,
        Command::FindBranch(args) => main_findbranch(args).await,
    }
}

async fn main_clone(opts: CloneArgs) -> Result<(), Box<dyn Error>> {
    let resolved = resolve_urls(opts.base_url.as_ref(), &opts.urls)?;

    let mut repotags = Vec::new();
    for url in &resolved {
        info!("=+============================================================");
        info!(" - {}", masked_url(url));
        let tag = clone_one(url, &opts).await?;
        info!(" - Tag: {}", tag);
        repotags.push(tag);
    }

    if let Some(path) = opts.tag_output_file {
        let tag = repotags
            .iter()
            .min_by(|a, b| natord::compare(a, b))
            .unwrap();
        let mut file = std::fs::File::create(&path)?;
        file.write_all(tag.as_bytes())?;
        debug!("Wrote tag {tag} to {path}");
    }
    Ok(())
}

async fn main_findbranch(opts: FindBranchArgs) -> Result<(), Box<dyn Error>> {
    let wanted_ref = opts
        .branches_starting_with
        .map(|b| format!("refs/heads/{}", b))
        .unwrap_or_else(|| "refs/heads/".to_string());

    let client = GitClient::new();
    let remote_repo = client.for_url(&Url::parse(&opts.repo_url)?);

    debug!("Listing remote refs (wanted ref: {:?})", wanted_ref);
    let refs = remote_repo.ls_refs(&[wanted_ref]).await?;

    let mut available_branches = HashMap::<&str, &RefInfo>::new();
    for r in &refs {
        if let Some(branchname) = r.refname.strip_prefix("refs/heads/") {
            available_branches.insert(branchname, r);
        }
    }

    let mut branch: Option<&RefInfo> =
        branch_fallback::resolve(&opts.branch, &opts.fallbacks, &available_branches);
    debug!("Found branch: {:?}", branch);
    if branch.is_none() && opts.default_branch.is_some() {
        branch = available_branches
            .get(opts.default_branch.as_ref().unwrap().as_str())
            .map(|v| &**v);
    }
    if let Some(branch) = branch {
        println!("{}", branch.refname.strip_prefix("refs/heads/").unwrap());
        Ok(())
    } else {
        Err("No suitable branch found".into())
    }
}
