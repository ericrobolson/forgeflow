mod agent;
use agent::*;

fn main() {
    println!("Hello, world!");

    let mut agent = Agent::new();
    let prompt = "What's 1+1";
    println!("Codex:");
    agent.codex(prompt).unwrap();

    println!("Claude:");
    agent.claude(prompt).unwrap();
}
