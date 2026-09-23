use forgeflow::agent::Agent;

fn main() {
    println!("Hello, world!");

    let prompt = "What's 1+1";
    println!("Codex:");
    println!("{}", Agent::codex().ask(prompt).unwrap());

    println!("Claude:");
    println!("{}", Agent::claude().ask(prompt).unwrap());

    println!("Opencode:");
    println!("{}", Agent::opencode().ask(prompt).unwrap());
}
