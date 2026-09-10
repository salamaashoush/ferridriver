use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() -> Result<()> {
  let url = std::env::args().nth(1).context("WebSocket URL required")?;
  let (mut socket, _) = tokio_tungstenite::connect_async(url).await?;
  let mut input = BufReader::new(tokio::io::stdin()).lines();
  let mut output = tokio::io::stdout();
  output.write_all(b"{\"connected\":true}\n").await?;
  output.flush().await?;
  loop {
    tokio::select! {
      line = input.next_line() => {
        let Some(line) = line? else { socket.close(None).await?; break; };
        socket.send(Message::Text(line.into())).await?;
      },
      frame = socket.next() => {
        match frame.transpose()? {
          Some(Message::Text(text)) => {
            output.write_all(text.as_bytes()).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
          },
          Some(Message::Close(_)) | None => break,
          _ => {},
        }
      },
    }
  }
  Ok(())
}
