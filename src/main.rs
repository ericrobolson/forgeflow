use forgeflow::agent::Agent;

fn main() {
    println!("{}", Agent::codex().ask("Say hello world").unwrap());
}
