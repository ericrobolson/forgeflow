use crate::{
    agent::{self},
    ops::*,
    project::{Project, REGISTERS_FILE, MEMORY_FILE},
    registers::{Memory, Registers},
    server::LocalServer,
    type_::TypeKind,
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
        check_steps(vec![], &self.steps).map(|_| ())
    }

    pub fn run(&self) -> Result<(), String> {
        self.check()?;

        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        let project = Project::initialize(&root).map_err(|e| e.to_string())?;
        let mut context = Context::new(project, self.agent_provider)?;
        run_steps(&mut context, &self.steps)
    }
}

/// Type-checks steps against a starting stack, returning the stack types afterwards.
pub fn check_steps(mut stack: Vec<TypeKind>, steps: &[Ops]) -> Result<Vec<TypeKind>, String> {
    for step in steps.iter() {
        // Stack shufflers keep their operands' types rather than widening them to Any.
        match step {
            Ops::Each(body) => {
                let found = match stack.pop() {
                    Some(TypeKind::List | TypeKind::Any) => None,
                    Some(other) => Some(format!("found {other:?}")),
                    None => Some("the stack is empty".to_string()),
                };
                if let Some(found) = found {
                    return Err(format!("`each` needs a list below its [ ] block, but {found}"));
                }
                let left = check_steps(vec![TypeKind::Any], body)
                    .map_err(|e| format!("in the [ ] block: {e}"))?;
                if !left.is_empty() {
                    return Err(format!(
                        "the [ ] block must use up its item and leave nothing, but leaves {} value(s)",
                        left.len()
                    ));
                }
                continue;
            }
            Ops::Dup | Ops::Swap | Ops::Drop => {
                let needed = step.inputs().len();
                if stack.len() < needed {
                    return Err(format!(
                        "{} needs {needed} value(s) on the stack, got {}",
                        step.name(),
                        stack.len()
                    ));
                }
                let top = stack.len() - 1;
                match step {
                    Ops::Dup => stack.push(stack[top]),
                    Ops::Swap => stack.swap(top, top - 1),
                    _ => {
                        stack.pop();
                    }
                }
                continue;
            }
            _ => {}
        }
        // Inputs are listed bottom to top, so the last one is on top of the stack.
        for input in step.inputs().into_iter().rev() {
            let head = stack.pop();
            if !head.is_some_and(|h| input.kind.accepts(h)) {
                let found = match head {
                    Some(h) => format!("found {h:?}"),
                    None => "the stack is empty".to_string(),
                };
                return Err(format!(
                    "`{}` needs {} ({:?}: {}), but {found}",
                    step.name(),
                    input.name,
                    input.kind,
                    input.description
                ));
            }
        }

        for output in step.outputs() {
            stack.push(output.kind);
        }
    }

    Ok(stack)
}

pub fn run_steps(context: &mut Context, steps: &[Ops]) -> Result<(), String> {
    for step in steps.iter() {
        step.process(context)?;
    }
    Ok(())
}

pub struct Context {
    pub stack: Vec<Value>,
    pub provider: agent::Provider,
    pub registers: Registers,
    pub memory: Memory,
    pub project: Project,
    /// The llama-server started for the agent, kept for the session and stopped on drop.
    pub server: Option<LocalServer>,
    /// Everything shown to the user since the last `take_printed`.
    pub printed: String,
    /// What `!` records as a register's source, such as the program that stored it.
    pub store_label: String,
    pub user_words: Vec<crate::user_words::UserWord>,
    pub local_frames: Vec<std::collections::HashMap<String, Value>>,
    pub call_depth: usize,
}

impl Context {
    pub fn new(project: Project, provider: agent::Provider) -> Result<Self, String> {
        let registers = Registers::open(project.folder().join(REGISTERS_FILE))?;
        let memory = Memory::open(project.folder().join(MEMORY_FILE))?;
        Ok(Self {
            stack: vec![],
            provider,
            registers,
            memory,
            project,
            server: None,
            printed: String::new(),
            store_label: "!".into(),
            user_words: vec![],
            local_frames: vec![],
            call_depth: 0,
        })
    }

    /// Shows text to the user and records it for the conversation history.
    pub fn say(&mut self, text: &str) {
        println!("{text}");
        self.printed.push_str(text);
        self.printed.push('\n');
    }

    pub fn take_printed(&mut self) -> String {
        std::mem::take(&mut self.printed)
    }

    pub fn stack_types(&self) -> Vec<TypeKind> {
        self.stack.iter().map(Value::kind).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow(steps: Vec<Ops>) -> WorkflowDefinition {
        WorkflowDefinition {
            name: "test".into(),
            steps,
            agent_provider: agent::Provider::Codex,
        }
    }

    #[test]
    fn check_pops_the_last_input_first() {
        // One string on the stack satisfies `contents` (top), leaving `file path` missing.
        let result = workflow(vec![Ops::StringLiteral("x".into()), Ops::WriteFile]).check();
        assert_eq!(
            result,
            Err("`write_file` needs path (String: file path), but the stack is empty".to_string())
        );
    }

    #[test]
    fn check_accepts_inputs_in_stack_order() {
        let steps = vec![
            Ops::StringLiteral("out.txt".into()),
            Ops::StringLiteral("contents".into()),
            Ops::WriteFile,
        ];
        assert_eq!(workflow(steps).check(), Ok(()));
    }

    #[test]
    fn stack_shufflers_preserve_types() {
        let steps = vec![
            Ops::StringLiteral("a".into()),
            Ops::IntLiteral(1),
            Ops::Swap,
            Ops::Dup,
        ];
        assert_eq!(
            check_steps(vec![], &steps),
            Ok(vec![TypeKind::Int, TypeKind::String, TypeKind::String])
        );
        assert!(check_steps(vec![], &[Ops::Swap]).is_err());
    }

    #[test]
    fn each_blocks_must_consume_their_item() {
        let files = || vec![Ops::StringLiteral("src".into()), Ops::ListFiles];
        let with = |body: Vec<Ops>| [files(), vec![Ops::Each(body)]].concat();
        assert_eq!(check_steps(vec![], &with(vec![Ops::Print])), Ok(vec![]));
        assert_eq!(
            check_steps(vec![], &with(vec![Ops::ReadFile])).unwrap_err(),
            "the [ ] block must use up its item and leave nothing, but leaves 1 value(s)"
        );
        assert!(check_steps(vec![], &with(vec![Ops::Add])).unwrap_err().starts_with("in the [ ] block"));
        assert!(check_steps(vec![], &[Ops::IntLiteral(1), Ops::Each(vec![Ops::Print])]).is_err());
    }

    #[test]
    fn fetched_values_type_check_at_runtime() {
        let steps = vec![Ops::Register(5), Ops::Fetch, Ops::ReadFile];
        assert_eq!(check_steps(vec![], &steps), Ok(vec![TypeKind::String]));
        let mismatch = vec![Ops::IntLiteral(3), Ops::ReadFile];
        assert!(check_steps(vec![], &mismatch).is_err());
    }
}
