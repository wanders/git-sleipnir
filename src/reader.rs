use bytes::{Bytes, BytesMut};
use futures::Stream;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

#[derive(Debug, PartialEq)]
pub enum GitPacketLine {
    Data(Bytes),
    Flush,
    Delimiter,
}

pub struct GitPacketLineStream<S> {
    inner: S,
    buffer: BytesMut,
    len: Option<usize>,
}

impl<S> GitPacketLineStream<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: BytesMut::new(),
            len: None,
        }
    }
}

impl<S, E> Stream for GitPacketLineStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: Into<std::io::Error>,
{
    type Item = Result<GitPacketLine, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            match this.len {
                None if this.buffer.len() >= 4 => {
                    match usize::from_str_radix(
                        std::str::from_utf8(&this.buffer.split_to(4)).map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidData, "Invalid hex length")
                        })?,
                        16,
                    ) {
                        Ok(0) => {
                            return Poll::Ready(Some(Ok(GitPacketLine::Flush)));
                        }
                        Ok(1) => {
                            return Poll::Ready(Some(Ok(GitPacketLine::Delimiter)));
                        }
                        Ok(n) if n >= 4 => {
                            this.len = Some(n - 4);
                        }
                        Ok(_) => {
                            return Poll::Ready(Some(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Invalid frame",
                            ))));
                        }
                        Err(_) => {
                            return Poll::Ready(Some(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Invalid hex length",
                            ))));
                        }
                    }
                }
                Some(n) if this.buffer.len() >= n => {
                    let data = this.buffer.split_to(n);
                    this.len = None;
                    return Poll::Ready(Some(Ok(GitPacketLine::Data(data.into()))));
                }
                _ => match Pin::new(&mut this.inner).poll_next(cx) {
                    Poll::Ready(Some(Ok(chunk))) => this.buffer.extend_from_slice(&chunk),
                    Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e.into()))),
                    Poll::Ready(None) => {
                        if this.buffer.is_empty() {
                            return Poll::Ready(None);
                        } else {
                            return Poll::Ready(Some(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "Unexpected EOF in packet line",
                            ))));
                        }
                    }
                    Poll::Pending => return Poll::Pending,
                },
            }
        }
    }
}

#[derive(Debug)]
pub enum SideBand {
    PackData(Bytes),
    Progress(String),
    ErrorMessage(String),
    Unknown(Bytes),
}

impl From<Bytes> for SideBand {
    fn from(data: bytes::Bytes) -> Self {
        if data.is_empty() {
            return SideBand::Unknown(data);
        }
        let channel = data[0];
        let payload = data.slice(1..);

        match channel {
            1 => SideBand::PackData(payload),
            2 => SideBand::Progress(String::from_utf8_lossy(&payload).into()),
            3 => SideBand::ErrorMessage(String::from_utf8_lossy(&payload).into()),
            _ => SideBand::Unknown(data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkt_line::PktLine;
    use bytes::Bytes;
    use futures::{stream, StreamExt};

    fn make_stream(data: &[&[u8]]) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
        let owned: Vec<_> = data
            .iter()
            .map(|&b| Ok(Bytes::copy_from_slice(b)))
            .collect();
        stream::iter(owned)
    }

    #[tokio::test]
    async fn test_flush_packet() {
        let data = vec![b"0000".as_ref()];
        let mut stream = GitPacketLineStream::new(make_stream(&data));

        let result = stream.next().await.unwrap().unwrap();
        matches!(result, GitPacketLine::Flush);
    }

    #[tokio::test]
    async fn test_delimiter_packet() {
        let data = vec![b"0001".as_ref()];
        let mut stream = GitPacketLineStream::new(make_stream(&data));

        let result = stream.next().await.unwrap().unwrap();
        matches!(result, GitPacketLine::Delimiter);
    }

    #[tokio::test]
    async fn test_data_packet_single_chunk() {
        let data = vec![b"000afoobar".as_ref()];
        let mut stream = GitPacketLineStream::new(make_stream(&data));

        let result = stream.next().await.unwrap().unwrap();
        match result {
            GitPacketLine::Data(d) => assert_eq!(&d[..], b"foobar"),
            _ => panic!("Expected Data packet"),
        }
    }

    #[tokio::test]
    async fn test_data_packet_split_chunks() {
        let data = vec![b"000a".as_ref(), b"foo".as_ref(), b"bar".as_ref()];
        let mut stream = GitPacketLineStream::new(make_stream(&data));

        let result = stream.next().await.unwrap().unwrap();
        match result {
            GitPacketLine::Data(d) => assert_eq!(&d[..], b"foobar"),
            _ => panic!("Expected Data packet"),
        }
    }

    #[tokio::test]
    async fn test_multiple_packet_lines() {
        let data = vec![
            b"000afoo123".as_ref(),
            b"000abar456".as_ref(),
            b"0000".as_ref(),
        ];
        let stream = GitPacketLineStream::new(make_stream(&data));

        let res: Vec<_> = stream.map(|x| x.unwrap()).collect().await;

        assert_eq!(
            res,
            [
                GitPacketLine::Data(Bytes::from("foo123")),
                GitPacketLine::Data(Bytes::from("bar456")),
                GitPacketLine::Flush,
            ]
        );
    }

    #[tokio::test]
    async fn test_sideband_parsing() {
        let data = Bytes::from_static(b"\x02remote message");
        let band: SideBand = data.into();

        match band {
            SideBand::Progress(msg) => assert_eq!(msg, "remote message"),
            _ => panic!("Expected Progress message"),
        }
    }

    #[tokio::test]
    async fn test_invalid_length_non_hex() {
        let data = vec![b"zzzzfoobar".as_ref()];
        let mut stream = GitPacketLineStream::new(make_stream(&data));
        let result = stream.next().await;
        assert!(result.unwrap().is_err());
    }

    #[tokio::test]
    async fn test_too_short_length_0002() {
        let data = vec![b"0002".as_ref()];
        let mut stream = GitPacketLineStream::new(make_stream(&data));
        let result = stream.next().await;
        assert!(result.unwrap().is_err());
    }

    #[tokio::test]
    async fn test_too_short_length_0003() {
        let data = vec![b"0003".as_ref()];
        let mut stream = GitPacketLineStream::new(make_stream(&data));
        let result = stream.next().await;
        assert!(result.unwrap().is_err());
    }

    #[tokio::test]
    async fn test_unexpected_eof() {
        let data = vec![b"000afoo".as_ref()];
        let mut stream = GitPacketLineStream::new(make_stream(&data));
        let result = stream.next().await;
        assert!(result.unwrap().is_err());
    }

    #[tokio::test]
    async fn test_multiple_packet_lines_builder() {
        let pkt = PktLine::new()
            .add(b"foo123")
            .add(b"bar456")
            .delimit()
            .add(b"baz789")
            .flush()
            .take();
        let data = vec![pkt.as_ref()];
        let stream = GitPacketLineStream::new(make_stream(&data));

        let res: Vec<_> = stream.map(|x| x.unwrap()).collect().await;

        assert_eq!(
            res,
            [
                GitPacketLine::Data(Bytes::from("foo123")),
                GitPacketLine::Data(Bytes::from("bar456")),
                GitPacketLine::Delimiter,
                GitPacketLine::Data(Bytes::from("baz789")),
                GitPacketLine::Flush,
            ]
        );
    }
}
