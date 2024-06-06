pub use anyhow::anyhow;

pub mod messages;
pub mod tasks;

pub fn post_server_check(server: String) -> anyhow::Result<()> {
    let result = ureq::get(&format!("http://{}/health", &server)).call();
    match result {
        Ok(_) => Ok(()),
        Err(_) => Err(anyhow!("post server doesn't work")),
    }
}
