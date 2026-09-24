use forgeflow::agent::Agent;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("Args: {:?}", args);
    println!("{}", Agent::codex().ask("Say hello world").unwrap());
}
