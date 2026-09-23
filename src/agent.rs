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

    pub fn ask(&self, prompt: &str) -> std::io::Result<String> {
        let mut child = Self::child(self.provider, prompt)?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let mut response = String::new();
        for line in io::BufReader::new(stdout).lines() {
            let line = line?;
            if let Some(text) = Self::response_text(self.provider, &line)? {
                response.push_str(&text);
            }
        }

        let status = child.wait()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "{} exited with {}",
                self.provider.to_str(), status
            )));
        }

        Ok(response)
    }

    fn response_text(provider: Provider, line: &str) -> io::Result<Option<String>> {
        let event: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} returned invalid JSON output: {error}", provider.to_str()),
            )
        })?;

        let text = match provider {
            Provider::Codex => event
                .get("item")
                .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("agent_message"))
                .and_then(|item| item.get("text"))
                .and_then(serde_json::Value::as_str),
            Provider::Claude => event.get("result").and_then(serde_json::Value::as_str),
            Provider::Opencode => event
                .get("type")
                .filter(|kind| kind.as_str() == Some("text"))
                .and_then(|_| event.get("part"))
                .and_then(|part| part.get("text"))
                .and_then(serde_json::Value::as_str),
        };

        Ok(text.map(str::to_owned))
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
