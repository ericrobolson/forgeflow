use forgeflow::{agent, ops::Ops, workflow::*};

fn main() {
    use Ops::*;
    let workflow = WorkflowDefinition {
        agent_provider: agent::Provider::Codex,
        name: "Example Workflow".to_string(),
        steps: vec![
            StringLiteral("Type in something to ask the llm:".into()),
            Print,
            ReadLine,
            StringLiteral("processing....".into()),
            Print,
            AskLlm,
            Print,
        ],
    };

    workflow.run().unwrap();
}
