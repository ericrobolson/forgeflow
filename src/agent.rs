use std::{
    io::{self, BufRead},
    process::{Command, Stdio},
};

pub struct Agent {}

impl Agent {
    pub fn new() -> Self {
        Self {}
    }

    pub fn claude(&self, prompt: &str) -> std::io::Result<()> {
        let mut child = Command::new("claude")
            .args(["-p", "--output-format", "stream-json", "--verbose", prompt])
            .stdout(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout was piped");
        for line in io::BufReader::new(stdout).lines() {
            println!("{}", line?);
        }

        let status = child.wait()?;
        if !status.success() {
            eprintln!("Claude exited with {status}");
        }

        Ok(())
    }

    pub fn codex(&self, prompt: &str) -> std::io::Result<()> {
        let mut child = Command::new("codex")
            .args(["exec", "--json", prompt])
            .stdout(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout was piped");
        for line in io::BufReader::new(stdout).lines() {
            println!("{}", line?); // Each JSON event, as it arrives
        }

        let status = child.wait()?;
        if !status.success() {
            eprintln!("Codex exited with {status}");
        }

        Ok(())
    }
}
