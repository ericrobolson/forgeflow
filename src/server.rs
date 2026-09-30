use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Server slots, each with its own prompt cache: translation and replies use separate
/// slots so a reply never evicts the translator's cached prompt.
pub const SLOTS: u64 = 2;
pub const TRANSLATE_SLOT: u32 = 0;
pub const ANSWER_SLOT: u32 = 1;

/// Ports tried from the configured one, so several sessions can run different models.
const PORT_RANGE: u16 = 10;
const HEALTH_TIMEOUT: Duration = Duration::from_secs(180);
const HEALTH_POLL: Duration = Duration::from_millis(500);

/// A llama-server child process serving one model over an OpenAI-compatible API.
#[derive(Debug)]
pub struct LocalServer {
    child: Option<Child>,
    pub model_path: PathBuf,
    pub port: u16,
}

impl LocalServer {
    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    /// Reuses a healthy server already serving `model_path` on `port`, or starts one.
    /// Reuses a server already running this model on `base_port` or one of the ports after
    /// it; otherwise starts one on the first free port. Ports serving other models, such as
    /// another session's, are skipped rather than disturbed.
    pub fn start(model_path: &Path, base_port: u16, ctx: u64, log_path: &Path) -> Result<Self, String> {
        let mut free = None;
        for port in base_port..base_port.saturating_add(PORT_RANGE) {
            if !health(port) {
                free = free.or(Some(port));
                continue;
            }
            if served_model(port).is_some_and(|served| same_model(&served, model_path)) {
                return Ok(Self {
                    child: None,
                    model_path: model_path.to_path_buf(),
                    port,
                });
            }
        }
        let port = free.ok_or(format!(
            "ports {base_port}-{} are all serving other models; stop one or change `port` in _forgeflow/config.json",
            base_port.saturating_add(PORT_RANGE - 1)
        ))?;
        let binary = find_binary().ok_or(
            "llama-server not found. Install it with `brew install llama.cpp`, or set FORGEFLOW_LLAMA_SERVER",
        )?;
        let log = File::create(log_path).map_err(|e| e.to_string())?;
        let err_log = log.try_clone().map_err(|e| e.to_string())?;
        eprintln!("Starting llama-server for {} (log: {})", model_path.display(), log_path.display());
        let child = Command::new(&binary)
            .arg("-m")
            .arg(model_path)
            .args(["--port", &port.to_string()])
            // llama-server divides the context among slots, so each slot gets `ctx`.
            .args(["-c", &(ctx * SLOTS).to_string()])
            // Offload every layer to the GPU; llama.cpp caps this at the model's layer count.
            .args(["-ngl", "999"])
            .args(["--parallel", &SLOTS.to_string()])
            .arg("--jinja")
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err_log)
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", binary.display()))?;
        let mut server = Self {
            child: Some(child),
            model_path: model_path.to_path_buf(),
            port,
        };
        server.wait_healthy(log_path)?;
        Ok(server)
    }

    fn wait_healthy(&mut self, log_path: &Path) -> Result<(), String> {
        let started = Instant::now();
        while started.elapsed() < HEALTH_TIMEOUT {
            if let Some(child) = self.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(format!(
                        "llama-server exited ({status}) while loading; last log lines:\n{}",
                        log_tail(log_path)
                    ));
                }
            }
            if health(self.port) {
                eprintln!("llama-server ready in {:.1}s", started.elapsed().as_secs_f64());
                return Ok(());
            }
            std::thread::sleep(HEALTH_POLL);
        }
        Err(format!("llama-server did not become healthy within {}s", HEALTH_TIMEOUT.as_secs()))
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn find_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("FORGEFLOW_LLAMA_SERVER") {
        return Some(path.into());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let built = home.join("local-models/llama.cpp/build/bin/llama-server");
    if built.is_file() {
        return Some(built);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("llama-server"))
        .find(|candidate| candidate.is_file())
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn health(port: u16) -> bool {
    agent(Duration::from_secs(2))
        .get(&format!("http://127.0.0.1:{port}/health"))
        .call()
        .is_ok()
}

/// The model file a running server reports, from `/v1/models`.
fn served_model(port: u16) -> Option<String> {
    let mut response = agent(Duration::from_secs(2))
        .get(&format!("http://127.0.0.1:{port}/v1/models"))
        .call()
        .ok()?;
    let body: serde_json::Value = response.body_mut().read_json().ok()?;
    body["data"][0]["id"].as_str().map(str::to_owned)
}

fn same_model(served: &str, path: &Path) -> bool {
    let served_name = Path::new(served).file_name();
    served_name.is_some() && served_name == path.file_name()
}

fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(15)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_served_model_by_file_name() {
        let path = Path::new("/models/qwen3-8b-q4km/Qwen3-8B-Q4_K_M.gguf");
        assert!(same_model("/Users/x/local-models/qwen3-8b-q4km/Qwen3-8B-Q4_K_M.gguf", path));
        assert!(same_model("Qwen3-8B-Q4_K_M.gguf", path));
        assert!(!same_model("Qwen3-4B-Q4_K_M.gguf", path));
    }
}
