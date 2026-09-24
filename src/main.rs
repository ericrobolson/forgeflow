fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("Args: {:?}", args);
    let working_dir = std::env::current_dir().unwrap();
    println!("working out of {:?}", working_dir);
}
