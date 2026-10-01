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

Chat is turn by turn. In the terminal, Left and Right move the input cursor, Option+Left and Option+Right move by word on macOS, and Up and Down browse input history for the current session. Ordinary messages go directly to the selected model, with no tools available. Prefix a request with `;` (leading spaces are ignored) to translate it into a ForgeFlow program, type-check it, and run it:

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

### Project-defined words

Put word definitions in any `.ff` file under `_forgeflow/`; nested folders are searched too. Files are loaded shallowest first and then by path, and definitions can call words from any discovered file. The generated `_forgeflow/user_words.txt` file is also loaded as ForgeFlow source and can be edited as plain text in Zed. A definition has a checked stack signature:

In the interactive prompt, initialize a word from a natural-language request with `; define ...`. ForgeFlow asks the model for one typed definition, validates it, saves it to `_forgeflow/user_words.txt`, and activates it immediately. For example, `; define a function called square which duplicates and multiplies its input` creates a persistent `square` word; call it with `; 3 square`. Invalid generated definitions are rejected without changing the saved file.

```forth
: double ( n Int -- result Int ) $n $n + swap drop ;
```

The signature lists named inputs and outputs from bottom to top. `$n` pushes a copy of the named input; use normal stack words to consume or reorder values. Supported types are `String`, `Int`, `Bool`, `List`, `Register`, `Address`, and `Any`. Definitions may call one another. Recursive calls must be in tail position; final-word calls are trampolined. Words are available to `.ff` programs, translated `;` requests, and as agent tools. Use `/reload` in chat to validate and activate edits; an invalid reload keeps the working set.

Declare a durable named cell with `variable score`. It pushes an `Address` that can be saved in registers or passed to user-defined words. `@` fetches the cell and `!` stores into it, for example `10 score ! score @ print`. Named cell values are stored in `_forgeflow/memory.json`; register persistence remains separate.

## Agent loop

The `agent` word (available in `.ff` programs) starts llama-server for the selected model (reusing one already serving it) and runs the loop in ForgeFlow itself. The model's tools are ForgeFlow words, with schemas generated from each word's typed inputs: `read_file`, `list_directory`, `reg_view`, `reg_search`, `reg_edit`, `reg_store`, and `emit`. File words are confined to the project root.

Tool results go into scratch registers (R4-R15), and the model receives a short receipt instead of the value, so small local models keep a small context. Text arguments accept `$R5` to pass a register's value. When scratch is full, the least recently used register is replaced and the receipt says so. Each run's transcript is saved in `_forgeflow/sessions/`.

`_forgeflow/config.json` holds the selected `model`, `image_model`, `ctx`, `port`, and `max_steps`. `image_model` defaults to `recraft/recraft-v4.1-flash`; `make-image` uses it with OpenRouter and reads the API key from `OPENROUTER_API_KEY`. For example, `"art/lantern.png" "A tiny brass fantasy lantern" make-image` generates one square image and returns its project-relative path. Pixel dimensions in the request are not honored; the model chooses its default resolution (Recraft produces roughly 1024×1024 images). Generated raster images are converted to PNG before saving. An agent run is limited to one image generation. Set `endpoint` (and `endpoint_model`) to use another OpenAI-compatible server, such as Ollama, instead of llama-server.

Environment overrides: `FORGEFLOW_MODELS_DIR` (default `~/local-models`) and `FORGEFLOW_LLAMA_SERVER` (the llama-server binary).
