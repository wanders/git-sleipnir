use std::error::Error;
use std::fmt;

use bytes::Bytes;
use futures::Stream;
use futures::StreamExt;
use futures::TryStreamExt;

use crate::local_repo::LocalRepo;
use crate::pkt_line::PktLine;
use crate::reader::GitPacketLine;
use crate::reader::GitPacketLineStream;
use crate::util::without_lf;
use crate::RefInfo;
use crate::ShallowInfo;

use log::{debug, error, info, trace, warn};
use url::Url;

pub struct GitClient {
    client: reqwest::Client,
}

#[derive(Debug)]
pub enum GitClientError {
    ConnectionError(reqwest::Error),
    ResponseError(String),
}

impl fmt::Display for GitClientError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            GitClientError::ConnectionError(e) => {
                write!(f, "Connection Error: '{}'", e)
            }
            GitClientError::ResponseError(m) => {
                write!(f, "Response Error: {}", m)
            }
        }
    }
}

impl Error for GitClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            GitClientError::ConnectionError(e) => Some(e),
            GitClientError::ResponseError(_) => None,
        }
    }
}

async fn consume_until_delimiter<S, E>(stream: &mut GitPacketLineStream<S>)
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: Into<std::io::Error>,
{
    while let Some(pkt) = stream.next().await {
        match pkt.expect("Stream error") {
            GitPacketLine::Data(_data) => {}
            GitPacketLine::Flush => {
                warn!("Unexpected flush");
                return;
            }
            GitPacketLine::Delimiter => {
                return;
            }
        }
    }
}

async fn handle_shallow_info<S, E>(stream: &mut GitPacketLineStream<S>) -> Vec<ShallowInfo>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: Into<std::io::Error>,
{
    let mut retval = Vec::new();

    while let Some(pkt) = stream.next().await {
        match pkt.expect("Stream error") {
            GitPacketLine::Data(data) => {
                if let Some(sha) = data.strip_prefix(b"shallow ") {
                    retval.push(ShallowInfo::Shallow(
                        String::from_utf8_lossy(sha).to_string(),
                    ));
                } else if let Some(sha) = data.strip_prefix(b"unshallow ") {
                    retval.push(ShallowInfo::NotShallow(
                        String::from_utf8_lossy(sha).to_string(),
                    ));
                } else {
                    warn!("Unexpected shallow: {}", String::from_utf8_lossy(&data));
                }
            }
            GitPacketLine::Flush => {
                warn!("Unexpected flush");
                break;
            }
            GitPacketLine::Delimiter => {
                break;
            }
        }
    }

    retval
}

impl GitClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                //.zstd(true)
                .read_timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap(),
        }
    }

    pub fn for_url(&self, url: &Url) -> GitRepoClient {
        let mut parsed = url.clone();

        /* This moves the username from url into reqwest object. */
        let username = parsed.username().to_string();
        let password = parsed.password().map(str::to_string);

        parsed.set_username("").ok();
        parsed.set_password(None).ok();

        let mut res = GitRepoClient::new(self.client.clone(), parsed);

        if let Some(password) = password {
            res.auth(&username, &password);
        }
        res
    }
}

pub struct GitRepoClient {
    client: reqwest::Client,
    url: Url,
    username: Option<String>,
    password: Option<String>,
}

impl GitRepoClient {
    fn new(client: reqwest::Client, url: Url) -> Self {
        GitRepoClient {
            client,
            url,
            username: None,
            password: None,
        }
    }

    fn auth(&mut self, username: &str, password: &str) {
        self.username = Some(username.to_string());
        self.password = Some(password.to_string());
    }

    pub async fn ls_refs<T: AsRef<str> + std::fmt::Display>(
        &self,
        ref_prefixes: &[T],
    ) -> Result<Vec<RefInfo>, GitClientError> {
        let mut retval: Vec<RefInfo> = Vec::new();

        let mut pkt = PktLine::new()
            .add(b"command=ls-refs\n")
            .add(b"agent=git-sleipnir/0\n")
            .add(b"object-format=sha1\n")
            .delimit()
            .add(b"peel\n");

        for p in ref_prefixes {
            let line = format!("ref-prefix {}\n", p);
            pkt = pkt.add(line.as_bytes())
        }

        let pkt = pkt.flush().take();

        let mut req = self
            .client
            .post(format!("{}/git-upload-pack", self.url))
            .header("Content-Type", "application/x-git-upload-pack-request")
            .header("Accept", "application/x-git-upload-pack-result")
            .header("Git-Protocol", "version=2")
            .body(pkt);
        if let Some(username) = &self.username {
            req = req.basic_auth(username, self.password.clone());
        }

        let res = req.send().await.map_err(GitClientError::ConnectionError)?;

        let status = res.status();
        if status.is_success() {
            let mut stream =
                GitPacketLineStream::new(res.bytes_stream().map_err(std::io::Error::other));

            while let Some(pkt) = stream.next().await {
                match pkt.expect("Stream error") {
                    GitPacketLine::Data(data) => {
                        let data = without_lf(data);
                        let parts: Vec<&[u8]> = data.split(|&b| b == b' ').collect();

                        if parts.len() == 2 {
                            retval.push(RefInfo {
                                sha: String::from_utf8_lossy(parts[0]).to_string(),
                                refname: String::from_utf8_lossy(parts[1]).to_string(),
                                peeled: None,
                            });
                        } else if parts.len() == 3 {
                            retval.push(RefInfo {
                                sha: String::from_utf8_lossy(parts[0]).to_string(),
                                refname: String::from_utf8_lossy(parts[1]).to_string(),
                                peeled: String::from_utf8_lossy(parts[2])
                                    .strip_prefix("peeled:")
                                    .map(|s| s.to_string()),
                            });
                        }
                    }
                    GitPacketLine::Flush => {
                        break;
                    }
                    GitPacketLine::Delimiter => {
                        warn!("Unexpected delimiter");
                    }
                }
            }
            Ok(retval)
        } else {
            let status = res.status();
            let url = res.url().clone();

            let max_len = 1024;
            let body = res
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".into());
            let preview = if body.len() > max_len {
                format!("{}...\n[truncated]", &body[..max_len])
            } else {
                body
            };

            error!("Request to {} failed with status {}", url, status);
            info!("Response text: {}", preview);
            Err(GitClientError::ResponseError(format!(
                "Request failed with status {}",
                status
            )))
        }
    }

    async fn upload_pack_req(&self, pkt: Vec<u8>) -> Result<reqwest::Response, reqwest::Error> {
        let mut req = self
            .client
            .post(format!("{}/git-upload-pack", self.url))
            .header("Content-Type", "application/x-git-upload-pack-request")
            .header("Accept", "application/x-git-upload-pack-result")
            .header("Git-Protocol", "version=2")
            .body(pkt);

        if let Some(username) = &self.username {
            req = req.basic_auth(username, self.password.clone());
        }

        req.send().await
    }

    pub async fn shallow_fetch(
        &self,
        local_repo: &LocalRepo,
        sha: &str,
        depth: usize,
    ) -> Result<(), reqwest::Error> {
        let mut pktbuilder = PktLine::new()
            .add(b"command=fetch")
            .add(b"agent=git-sleipnir/0\n")
            .add(b"object-format=sha1")
            .delimit()
            .add(format!("want {}", sha).as_bytes());

        for shallowsha in local_repo.get_shallow_shas().await.iter() {
            pktbuilder = pktbuilder.add(format!("shallow {}", shallowsha).as_bytes());
        }

        let pkt = pktbuilder
            .add(format!("deepen {}", depth).as_bytes())
            .add(b"include-tag")
            .add(b"done\n")
            .flush()
            .take();

        let res = self.upload_pack_req(pkt).await?;

        let status = res.status();
        if status.is_success() {
            let mut stream =
                GitPacketLineStream::new(res.bytes_stream().map_err(std::io::Error::other));

            let mut shallow_info = Vec::new();
            while let Some(pkt) = stream.next().await {
                match pkt.expect("Stream error") {
                    GitPacketLine::Data(data) => match without_lf(data).as_ref() {
                        b"packfile" => {
                            local_repo.handle_packfile(&mut stream).await.unwrap();
                            break;
                        }
                        b"shallow-info" => {
                            shallow_info = handle_shallow_info(&mut stream).await;
                        }
                        data => {
                            debug!("Ignoring unknown gitline: {data:?}");
                            consume_until_delimiter(&mut stream).await;
                        }
                    },
                    GitPacketLine::Flush => {
                        break;
                    }
                    GitPacketLine::Delimiter => {
                        warn!("Unexpected delimiter");
                    }
                }
            }
            local_repo.update_shallow_file(&shallow_info).await;
        } else {
            let body = res.text().await?;
            error!("Unexpected HTTP status: {}", status);
            trace!("Body: {body}");
        }
        Ok(())
    }
}
