use std::io::{self, Write};

pub fn read_line() -> Result<String, String> {
    let mut answer = String::new();
    match io::stdin().read_line(&mut answer) {
        Ok(_) => Ok(answer),
        Err(e) => Err(format!("{:?}", e)),
    }
}

pub fn confirmation(prompt: &str) -> bool {
    println!("{prompt}");
    print!("Enter y|n\n> ");
    io::stdout().flush().unwrap();

    while let Ok(input) = read_line() {
        let input = input.trim();
        if input == "y" {
            return true;
        }

        if input == "n" {
            return false;
        }

        println!("Unknown input.");
        print!("Enter y|n\n> ");
        io::stdout().flush().unwrap();
    }

    false
}
