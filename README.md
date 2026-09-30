# ForgeFlow

Concatenative agentic orchestration workflow builder.


## Installation

Run `cargo build --release`
Then `cp target/release/forgeflow ~/bin/forgeflow`
Then add 
```
forgeflow() { ~/bin/forgeflow $@ }
export -f forgeflow
```

to your zshrc

## Use

```
forgeflow              # chat in the current directory
forgeflow models       # local models, with fit and speed estimates for this machine
forgeflow pull <id>    # resumable download, sha256-verified, into ~/local-models
forgeflow use <id>     # select the model
forgeflow file.ff      # run a ForgeFlow program
```

## Chat

Chat is turn by turn. Ordinary messages go directly to the selected model, with no tools available. Prefix a request with `;` (leading spaces are ignored) to translate it into a ForgeFlow program, type-check it, and run it:

```
; list files in src
```

Every other message stays in chat. ForgeFlow code can call its words directly, including `agent` when you want the model to use project tools:

```
> which lines of src/device.rs mention M4 Pro?
→ "src/device.rs" read_file "M4 Pro" grep print
94	        ("M4 Pro", 273.0),
translate 401 ms · run 0 ms
```

The chat assistant receives conversational history and no function tools. Semicolon-prefixed requests use a constrained translator; its programs are checked before running, and invalid programs get one correction attempt. Type `; help` to see the available vocabulary.

The code vocabulary includes file and directory reads, register memory, arithmetic, and `[ ... ] each` loops. Programs can also use `agent` to start the tool-calling loop. Ordinary chat messages never run these words implicitly.

## Agent loop

The `agent` word (available in `.ff` programs) starts llama-server for the selected model (reusing one already serving it) and runs the loop in ForgeFlow itself. The model's tools are ForgeFlow words, with schemas generated from each word's typed inputs: `read_file`, `list_directory`, `reg_view`, `reg_search`, `reg_edit`, `reg_store`, and `emit`. File words are confined to the project root.

Tool results go into scratch registers (R4-R15), and the model receives a short receipt instead of the value, so small local models keep a small context. Text arguments accept `$R5` to pass a register's value. When scratch is full, the least recently used register is replaced and the receipt says so. Each run's transcript is saved in `_forgeflow/sessions/`.

`_forgeflow/config.json` holds the selected `model`, `ctx`, `port`, and `max_steps`. Set `endpoint` (and `endpoint_model`) to use another OpenAI-compatible server, such as Ollama, instead of llama-server.

Environment overrides: `FORGEFLOW_MODELS_DIR` (default `~/local-models`) and `FORGEFLOW_LLAMA_SERVER` (the llama-server binary).
