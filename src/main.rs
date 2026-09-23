mod agent;
use agent::*;

fn main() {
    println!("Hello, world!");

    let prompt = "What's 1+1";
    println!("Codex:");
    Agent::codex().ask(prompt).unwrap();

    println!("Claude:");
    Agent::claude().ask(prompt).unwrap();

    println!("Opencode:");
    Agent::opencode().ask(prompt).unwrap();
}
