# Rust async cheatsheet
- async fn returns impl Future; .await polls it
- tokio::spawn needs 'static + Send
- select! races futures; join! waits for all
- Use tokio::sync::mpsc for channels between tasks
