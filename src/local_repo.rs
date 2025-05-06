use futures::Stream;
use futures::StreamExt;
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::process::Stdio;

use bytes::Bytes;

use log::warn;

use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::{Child, Command};

use crate::reader::GitPacketLine;
use crate::reader::GitPacketLineStream;
use crate::reader::SideBand;

use crate::util::read_lines_to_set;
use crate::util::write_lines_from_set;

use crate::ShallowInfo;

#[derive(Debug)]
pub enum LocalRepoError {
    AlreadyExists(PathBuf),
    DirectoryCreationError((PathBuf, std::io::Error)),
    ExternalGitCommandSpawnFailure(std::io::Error),
    ExternalGitCommandError(ExitStatus),
}

impl fmt::Display for LocalRepoError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LocalRepoError::AlreadyExists(p) => {
                write!(f, "Directory '{}' already exists.", p.display())
            }
            LocalRepoError::DirectoryCreationError((p, e)) => {
                write!(f, "Could not create directory '{}': {}", p.display(), e)
            }
            LocalRepoError::ExternalGitCommandSpawnFailure(e) => {
                write!(f, "Could not spawn git process: {}", e)
            }
            LocalRepoError::ExternalGitCommandError(es) => {
                write!(f, "External git process failed: {}", es)
            }
        }
    }
}

impl Error for LocalRepoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            LocalRepoError::AlreadyExists(_) => None,
            LocalRepoError::DirectoryCreationError((_, e)) => Some(e),
            LocalRepoError::ExternalGitCommandSpawnFailure(e) => Some(e),
            LocalRepoError::ExternalGitCommandError(_) => None,
        }
    }
}

type Result<T> = std::result::Result<T, LocalRepoError>;

pub struct LocalRepo {
    path: PathBuf,
}

async fn wait_result<T, U: FnOnce() -> T>(mut child: Child, func: U) -> Result<T> {
    let es = child.wait().await.expect("Waiting for git command");
    if es.success() {
        Ok(func())
    } else {
        Err(LocalRepoError::ExternalGitCommandError(es))
    }
}

impl LocalRepo {
    pub async fn init_new(path: &Path) -> Result<Self> {
        std::fs::create_dir(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => LocalRepoError::AlreadyExists(path.into()),
            _ => LocalRepoError::DirectoryCreationError((path.into(), e)),
        })?;

        wait_result(
            Command::new("git")
                .arg("init")
                .arg(path)
                .spawn()
                .map_err(LocalRepoError::ExternalGitCommandSpawnFailure)?,
            || Self { path: path.into() },
        )
        .await
    }

    pub async fn get_shallow_shas(&self) -> HashSet<String> {
        let path = self.path.join(".git/shallow");
        read_lines_to_set(&path)
            .await
            .unwrap_or_else(|_e| HashSet::new())
    }

    pub async fn update_shallow_file(&self, info: &Vec<ShallowInfo>) {
        let mut shallow_shas = self.get_shallow_shas().await;

        for e in info {
            match e {
                ShallowInfo::Shallow(sha) => shallow_shas.insert(sha.into()),
                ShallowInfo::NotShallow(sha) => shallow_shas.remove(sha),
            };
        }

        let path = self.path.join(".git/shallow");
        write_lines_from_set(&path, &shallow_shas).await.unwrap();
    }

    pub async fn update_ref(&self, refname: &str, sha: &str) -> Result<()> {
        wait_result(
            self.git()
                .arg("update-ref")
                .arg(refname)
                .arg(sha)
                .spawn()
                .map_err(LocalRepoError::ExternalGitCommandSpawnFailure)?,
            || (),
        )
        .await
    }

    pub async fn update_head(&self, refname: &str) -> Result<()> {
        wait_result(
            self.git()
                .arg("symbolic-ref")
                .arg("HEAD")
                .arg(refname)
                .spawn()
                .map_err(LocalRepoError::ExternalGitCommandSpawnFailure)?,
            || (),
        )
        .await
    }

    pub async fn checkout_head(&self) -> Result<()> {
        wait_result(
            self.git()
                .arg("checkout")
                .arg("HEAD")
                .spawn()
                .map_err(LocalRepoError::ExternalGitCommandSpawnFailure)?,
            || (),
        )
        .await
    }

    pub async fn rev_list(&self, sha: &str) -> Result<Vec<String>> {
        let mut cmd = self
            .git()
            .arg("rev-list")
            .arg(sha)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(LocalRepoError::ExternalGitCommandSpawnFailure)?;

        let stdout = cmd.stdout.take().expect("Failed to capture stdout");
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();

        let mut result = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            result.push(line);
        }

        wait_result(cmd, || result).await
    }

    fn git(&self) -> tokio::process::Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C");
        cmd.arg(&self.path);
        cmd
    }

    pub async fn handle_packfile<S, E>(&self, stream: &mut GitPacketLineStream<S>) -> Result<()>
    where
        S: Stream<Item = std::result::Result<Bytes, E>> + Unpin,
        E: Into<std::io::Error>,
    {
        let mut index_pack_cmd = self
            .git()
            .arg("index-pack")
            .arg("--stdin")
            .arg("-v")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(LocalRepoError::ExternalGitCommandSpawnFailure)?;

        let mut stdin = index_pack_cmd
            .stdin
            .take()
            .expect("child didn't have a stdin");

        while let Some(pkt) = stream.next().await {
            match pkt.expect("Stream error") {
                GitPacketLine::Data(data) => {
                    let d: SideBand = data.into();
                    match d {
                        SideBand::PackData(payload) => {
                            stdin.write_all(&payload).await.expect("write");
                        }
                        SideBand::Progress(msg) => {
                            print!("{}", msg);
                            std::io::stdout().flush().unwrap();
                        }
                        SideBand::ErrorMessage(msg) => {
                            println!("remote: {}", msg);
                        }
                        SideBand::Unknown(b) => {
                            let first_40 = b.slice(0..std::cmp::min(40, b.len()));
                            warn!("unknown sideband channel data: {first_40:?}");
                        }
                    }
                }
                GitPacketLine::Flush => {
                    break;
                }
                GitPacketLine::Delimiter => {
                    warn!("Unexpected delimiter");
                    break;
                }
            }
        }

        wait_result(index_pack_cmd, || ()).await
    }
}
