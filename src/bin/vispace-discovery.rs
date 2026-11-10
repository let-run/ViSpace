//! Chrome native-messaging host for DNS-SD discovery only.
#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    use anyhow::ensure;
    use std::io::{Read, Write};
    let mut input = std::io::stdin().lock();
    let mut length = [0; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_ne_bytes(length) as usize;
    ensure!(
        (1..=1024).contains(&length),
        "Invalid discovery request size"
    );
    let mut request = vec![0; length];
    input.read_exact(&mut request)?;
    let request: serde_json::Value = serde_json::from_slice(&request)?;
    ensure!(request["operation"] == "discover", "Unsupported operation");
    let response = match vispace::discovery::nearby() {
        Ok(nodes) => serde_json::json!({"nodes":nodes}),
        Err(error) => serde_json::json!({"error":error.to_string()}),
    };
    let response = serde_json::to_vec(&response)?;
    let mut output = std::io::stdout().lock();
    output.write_all(&(response.len() as u32).to_ne_bytes())?;
    output.write_all(&response)?;
    output.flush()?;
    Ok(())
}
#[cfg(target_arch = "wasm32")]
fn main() {}
