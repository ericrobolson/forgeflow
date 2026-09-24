use crate::{
    agent::{self, Agent},
    type_::Type,
    util,
    value::Value,
    workflow::*,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Ops {
    SpecifyAgent(agent::Provider),
    StringLiteral(String),
    AllCliArgs,
    WriteFile,
    /// Reads a line from stdin
    ReadLine,
    AskLlm,
    Print,
}
impl Ops {
    pub fn process(&self, context: &mut Context) -> Result<(), String> {
        let stack = &mut context.stack;
        match self {
            Ops::AllCliArgs => todo!(),
            Ops::WriteFile => todo!(),
            Ops::ReadLine => {
                let answer = util::read_line()?;
                stack.push(Value::String(answer));
            }
            Ops::AskLlm => {
                let question = stack
                    .pop()
                    .ok_or("Expected a question".to_string())?
                    .expect_string()?;
                let agent = Agent::new(context.provider);
                let answer = agent.ask(&question).map_err(|e| e.to_string())?;
                stack.push(Value::String(answer));
            }
            Ops::Print => {
                let value = stack
                    .pop()
                    .ok_or("Expected a value".to_string())?
                    .expect_string()?;
                println!("{}", value);
            }
            Ops::StringLiteral(s) => stack.push(Value::String(s.clone())),
            Ops::SpecifyAgent(provider) => todo!(),
        }
        Ok(())
    }

    /// The things the given step requires on the stack.
    pub fn inputs(&self) -> Vec<Type> {
        match self {
            Ops::WriteFile => vec![Type::str("file path"), Type::str("contents")],
            Ops::AskLlm => vec![Type::str("the question to ask")],
            Ops::Print => vec![Type::str("Can print a string")],
            Ops::ReadLine | Ops::StringLiteral(_) | Ops::SpecifyAgent(_) | Ops::AllCliArgs => {
                vec![]
            }
        }
    }

    /// The things the given step puts on the stack.
    pub fn outputs(&self) -> Vec<Type> {
        match self {
            Ops::AllCliArgs => vec![Type::str("the args as a string")],
            Ops::ReadLine => vec![Type::str("A line the user wrote")],
            Ops::AskLlm => vec![Type::str("The response")],
            Ops::StringLiteral(_) => vec![Type::str("The string literal")],
            Ops::SpecifyAgent(_) | Ops::WriteFile | Ops::Print => {
                vec![]
            }
        }
    }
}
