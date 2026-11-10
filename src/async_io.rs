//! Native async I/O host. Browser builds use a browser executor and storage adapter.
use anyhow::Result;
#[cfg(not(target_arch = "wasm32"))]
use anyhow::ensure;
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
pub fn runtime() -> Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
        std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .max_blocking_threads(4)
                .thread_name("inventory-io")
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!(e.clone()))
}
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn(work: impl std::future::Future<Output = ()> + Send + 'static) -> Result<()> {
    runtime()?.spawn(work);
    Ok(())
}
#[cfg(target_arch = "wasm32")]
pub fn spawn(work: impl std::future::Future<Output = ()> + 'static) -> Result<()> {
    wasm_bindgen_futures::spawn_local(work);
    Ok(())
}
#[cfg(not(target_arch = "wasm32"))]
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| anyhow::anyhow!("I/O task failed: {e}"))?
}
#[cfg(target_arch = "wasm32")]
pub async fn blocking<T: 'static>(work: impl FnOnce() -> Result<T> + 'static) -> Result<T> {
    drop(work);
    anyhow::bail!("This browser build needs a browser I/O adapter")
}
#[cfg(not(target_arch = "wasm32"))]
pub async fn download_pdf(source: &str, destination: &Path) -> Result<String> {
    let url = if let Some(tail) = source.strip_prefix("http://") {
        format!("https://{tail}")
    } else {
        source.to_owned()
    };
    ensure!(url.starts_with("https://"), "Use an HTTP or HTTPS URL");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" {
                attempt.error("Download redirected to insecure HTTP")
            } else if attempt.previous().len() >= 10 {
                attempt.error("Too many redirects")
            } else {
                attempt.follow()
            }
        }))
        .build()?;
    let mut response = client.get(url).send().await?.error_for_status()?;
    ensure!(
        response.content_length().is_none_or(|n| n <= 30_000_000),
        "Response is not a PDF or exceeds 30 MB"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        append_pdf_chunk(&mut bytes, &chunk)?;
    }
    ensure!(
        bytes.starts_with(b"%PDF-"),
        "Response is not a PDF or exceeds 30 MB"
    );
    if let Err(error) = tokio::fs::write(destination, &bytes).await {
        let _ = tokio::fs::remove_file(destination).await;
        return Err(error.into());
    }
    Ok(destination.to_string_lossy().into())
}
#[cfg(target_arch = "wasm32")]
pub async fn download_pdf(_source: &str, _destination: &Path) -> Result<String> {
    anyhow::bail!("This browser build needs a browser download/storage adapter")
}
#[cfg(not(target_arch = "wasm32"))]
fn append_pdf_chunk(bytes: &mut Vec<u8>, chunk: &[u8]) -> Result<()> {
    ensure!(
        chunk.len() <= 30_000_000usize.saturating_sub(bytes.len()),
        "Response is not a PDF or exceeds 30 MB"
    );
    bytes.extend_from_slice(chunk);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_download_limit_is_enforced_before_appending() {
        let mut bytes = b"%PDF-1.7".to_vec();
        append_pdf_chunk(&mut bytes, b"test").unwrap();
        let before = bytes.clone();
        assert!(append_pdf_chunk(&mut bytes, &vec![0; 30_000_000]).is_err());
        assert_eq!(bytes, before);
    }
}
