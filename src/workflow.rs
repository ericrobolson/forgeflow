use crate::{
    agent::{self},
    ops::*,
    value::Value,
};

#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowDefinition {
    pub name: String,
    pub steps: Vec<Ops>,
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
    pub stack: Vec<Value>,
    pub provider: agent::Provider,
}
