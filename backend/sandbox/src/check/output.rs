use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::debug;

use crate::SandboxedChild;

pub const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

const TRUNCATED: &str = "\n[ekaci: output truncated]\n";

pub(crate) async fn capture(child: &mut SandboxedChild, limit: usize) -> (Vec<u8>, Vec<u8>) {
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    tokio::join!(read_capped(stdout, limit), read_capped(stderr, limit))
}

async fn read_capped<R: AsyncRead + Unpin>(stream: Option<R>, limit: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let Some(mut stream) = stream else {
        return kept;
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut truncated = false;
    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                debug!("reading sandbox output failed: {e}");
                break;
            },
        };
        let room = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
        truncated |= n > room;
    }
    if truncated {
        kept.extend_from_slice(TRUNCATED.as_bytes());
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keeps_prefix_and_marks_truncation() {
        let data = vec![b'x'; 100];
        let out = read_capped(Some(&data[..]), 10).await;
        assert!(out.starts_with(&[b'x'; 10]));
        assert!(out.ends_with(TRUNCATED.as_bytes()));
        assert_eq!(read_capped(Some(&data[..]), 100).await, data);
        assert!(read_capped(None::<&[u8]>, 1).await.is_empty());
    }
}
