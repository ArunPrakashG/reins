use reins_proto::{Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct RpcClient {
    socket_path: std::path::PathBuf,
}

impl RpcClient {
    pub fn new(socket_path: std::path::PathBuf) -> Self {
        Self { socket_path }
    }

    pub async fn send(&self, req: Request) -> anyhow::Result<Response> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        let (read_half, mut write_half) = stream.into_split();
        let mut msg = serde_json::to_string(&req)?;
        msg.push('\n');
        write_half.write_all(msg.as_bytes()).await?;

        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        Ok(serde_json::from_str(&line)?)
    }
}
