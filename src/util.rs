use tokio::fs::File;
use tokio::io::BufReader;

use bytes::Bytes;

use std::collections::HashSet;
use std::path::Path;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufWriter;

pub async fn read_lines_to_set(path: &Path) -> std::io::Result<HashSet<String>> {
    let file = File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let mut set = HashSet::new();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            set.insert(trimmed.to_string());
        }
    }

    Ok(set)
}

pub async fn write_lines_from_set(path: &Path, set: &HashSet<String>) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");

    {
        let file = File::create(&tmp_path).await?;
        let mut writer = BufWriter::new(file);

        let mut lines: Vec<&str> = set.iter().map(|s| s.as_str()).collect();
        lines.sort_unstable();

        for line in lines {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
        }
        writer.flush().await?;
    }

    tokio::fs::rename(&tmp_path, path).await
}

pub fn without_lf(bytes: Bytes) -> Bytes {
    if bytes.ends_with(b"\n") {
        bytes.slice(..bytes.len() - 1)
    } else {
        bytes
    }
}
