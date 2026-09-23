use std::{
    io::{self, BufRead},
    process::{Child, Command, Stdio},
};

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Error(String),
    Text(String),
    Started,
    Finished,
    Metadata(serde_json::Value),
    Unknown(serde_json::Value),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Provider {
    Opencode,
    Codex,
    Claude,
}
impl Provider {
    pub fn to_str(&self) -> &'static str {
        match self {
            Provider::Opencode => "opencode",
            Provider::Codex => "codex",
            Provider::Claude => "claude",
        }
    }

    pub fn command(&self) -> Command {
        match self {
            Provider::Opencode => Command::new("opencode"),
            Provider::Codex => Command::new("codex"),
            Provider::Claude => Command::new("claude"),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Agent {
    provider: Provider,
}

impl Agent {
    pub fn new(provider: Provider) -> Self {
        Self { provider }
    }

    pub fn codex() -> Self {
        Self::new(Provider::Codex)
    }

    pub fn claude() -> Self {
        Self::new(Provider::Claude)
    }

    pub fn opencode() -> Self {
        Self::new(Provider::Opencode)
    }

    pub fn ask(&self, prompt: &str) -> std::io::Result<()> {
        let mut child = Self::child(self.provider, prompt)?;
        let stdout = child.stdout.take().expect("stdout was piped");
        for line in io::BufReader::new(stdout).lines() {
            println!("{}", line?);
        }

        let status = child.wait()?;
        if !status.success() {
            eprintln!("{} exited with {}", self.provider.to_str(), status);
        }

        Ok(())
    }

    fn child(provider: Provider, prompt: &str) -> Result<Child, std::io::Error> {
        let mut command = provider.command();
        let command = match provider {
            Provider::Opencode => command.args(["run", "--format", "json", prompt]),
            Provider::Codex => command.args(["exec", "--json", prompt]),
            Provider::Claude => {
                command.args(["-p", "--output-format", "stream-json", "--verbose", prompt])
            }
        };

        command.stdout(Stdio::piped()).spawn()
    }
}
