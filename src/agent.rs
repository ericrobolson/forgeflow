use std::{
    io::{self, BufRead},
    process::{Child, Command, Stdio},
};

pub struct Agent {}

impl Agent {
    pub fn new() -> Self {
        Self {}
    }

    fn call(&self, command: &mut Command, kind: &str, prompt: &str) -> std::io::Result<()> {
        let mut child = command.stdout(Stdio::piped()).spawn()?;

        let stdout = child.stdout.take().expect("stdout was piped");
        for line in io::BufReader::new(stdout).lines() {
            println!("{}", line?);
        }

        let status = child.wait()?;
        if !status.success() {
            eprintln!("{} exited with {}", kind, status);
        }

        Ok(())
    }

    pub fn claude(&self, prompt: &str) -> std::io::Result<()> {
        self.call(
            Command::new("claude").args([
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                prompt,
            ]),
            "claude",
            prompt,
        )
    }

    pub fn opencode(&self, prompt: &str) -> std::io::Result<()> {
        self.call(
            Command::new("opencode").args(["run", "--format", "json", prompt]),
            "opencode",
            prompt,
        )
    }

    pub fn codex(&self, prompt: &str) -> std::io::Result<()> {
        self.call(
            Command::new("codex").args(["exec", "--json", prompt]),
            "codex",
            prompt,
        )
    }
}
