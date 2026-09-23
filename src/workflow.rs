use crate::agent::{self, Agent};
use std::io::{self};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowDefinition {
    pub name: String,
    pub steps: Vec<StepDefinition>,
    pub agent_provider: agent::Provider,
}
impl WorkflowDefinition {
    pub fn check(&self) -> Result<(), String> {
        let mut stack = vec![];
        for step in self.steps.iter() {
            for input in step.inputs() {
                let head = stack.pop();
                let kind_formatted = format!("{:?}", input.kind);
                if head != Some(input.kind) {
                    let unwrapped = match head {
                        Some(h) => {
                            format!("{:?}", h)
                        }
                        None => "Got nothing!".to_string(),
                    };
                    return Err(format!(
                        "{} expected {}, got {}",
                        input.description, kind_formatted, unwrapped
                    ));
                }
            }

            for output in step.outputs() {
                stack.push(output.kind);
            }
        }

        Ok(())
    }

    pub fn run(&self) -> Result<(), String> {
        self.check()?;

        let mut context = Context {
            stack: vec![],
            provider: self.agent_provider,
        };
        for step in self.steps.iter() {
            step.process(&mut context)?;
        }

        Ok(())
    }
}

pub struct Context {
    stack: Vec<Value>,
    provider: agent::Provider,
}

/// A specification for a type.
#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    /// The kind of the type.
    pub kind: TypeKind,
    /// The description for the type.
    pub description: String,
}
impl Type {
    /// Creates a string type
    pub fn str(description: &str) -> Self {
        Self {
            kind: TypeKind::String,
            description: description.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
}
impl Value {
    pub fn expect_string(&self) -> Result<String, String> {
        match self {
            Value::String(s) => Ok(s.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepDefinition {
    SpecifyAgent(agent::Provider),
    StringLiteral(String),
    AllCliArgs,
    WriteFile,
    /// Reads a line from stdin
    ReadLine,
    AskLlm,
    Print,
}
impl StepDefinition {
    pub fn process(&self, context: &mut Context) -> Result<(), String> {
        let stack = &mut context.stack;
        match self {
            StepDefinition::AllCliArgs => todo!(),
            StepDefinition::WriteFile => todo!(),
            StepDefinition::ReadLine => {
                let mut answer = String::new();
                match io::stdin().read_line(&mut answer) {
                    Ok(_) => {}
                    Err(e) => return Err(format!("{:?}", e)),
                }
                stack.push(Value::String(answer));
            }
            StepDefinition::AskLlm => {
                let question = stack
                    .pop()
                    .ok_or("Expected a question".to_string())?
                    .expect_string()?;
                let agent = Agent::new(context.provider);
                let answer = agent.ask(&question).map_err(|e| e.to_string())?;
                stack.push(Value::String(answer));
            }
            StepDefinition::Print => {
                let value = stack
                    .pop()
                    .ok_or("Expected a value".to_string())?
                    .expect_string()?;
                println!("{}", value);
            }
            StepDefinition::StringLiteral(s) => stack.push(Value::String(s.clone())),
            StepDefinition::SpecifyAgent(provider) => todo!(),
        }
        Ok(())
    }

    /// The things the given step requires on the stack.
    pub fn inputs(&self) -> Vec<Type> {
        match self {
            StepDefinition::WriteFile => vec![Type::str("file path"), Type::str("contents")],
            StepDefinition::AskLlm => vec![Type::str("the question to ask")],
            StepDefinition::Print => vec![Type::str("Can print a string")],
            StepDefinition::ReadLine
            | StepDefinition::StringLiteral(_)
            | StepDefinition::SpecifyAgent(_)
            | StepDefinition::AllCliArgs => vec![],
        }
    }

    /// The things the given step puts on the stack.
    pub fn outputs(&self) -> Vec<Type> {
        match self {
            StepDefinition::AllCliArgs => vec![Type::str("the args as a string")],
            StepDefinition::ReadLine => vec![Type::str("A line the user wrote")],
            StepDefinition::AskLlm => vec![Type::str("The response")],
            StepDefinition::StringLiteral(_) => vec![Type::str("The string literal")],
            StepDefinition::SpecifyAgent(_) | StepDefinition::WriteFile | StepDefinition::Print => {
                vec![]
            }
        }
    }
}
