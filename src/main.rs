use forgeflow::agent::Agent;

fn main() {
    Agent::codex().ask("Say hello world").unwrap();
}
