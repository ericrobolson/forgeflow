- [ ] Add ability to execute prompt real time, outputting stuff when you can or when user stops typing.

- [x] Build out a definition for a Forth like, strongly and statically typed, concatenative language for interacting with agents
- [x] Build out a runtime to process it
- [x] Add a project concept + initialization.
- [x] Add ability to parse + validate + load text files
- [ ] In main menu, add ability to run workflows
- [ ] Add loading of source files or scaffolding of them + durable workflow state for things like agentic pipelines
- [ ] Add swap/swizzle/etc (dup, drop, swap done)
- [x] Add memory (16 registers: R0-R3 durable, R4-R15 scratch)
- [ ] Add function definitions
- [ ] Add a visualizer/editor so as you move your cursor over a word it'll show the stack and any errors visually
- [x] Build out a parser to parse programs
- [x] Host-run agent loop: words exposed as tools, results in registers
- [x] Chat: each message translates to one grammar-constrained program, run deterministically
- [x] Loops: `[ ... ] each` over lists
- [ ] Conditionals
- [ ] Run `each` items concurrently on spare server slots
- [ ] Smaller translator model option, separate from the answer model
- [x] Local models: device probe, catalog with fit/speed estimates, resumable verified download, managed llama-server
- [ ] Anthropic Messages backend
- [ ] CLI backends (claude/codex/opencode) behind the loop, with tool calls as text
- [ ] Text tool-call format for models without native tool calling
- [ ] Write queue words (queue_write, pending, approve, apply) ported from flowforge
- [ ] Archive words (find_symbols, show_symbol, reindex) ported from flowforge
- [ ] `bench` word: measure real tokens/s and replace the estimate
- [ ] Batch compaction of old `reg_view` results when the context fills


## User-defined words

- [ ] Add Forth-style word definitions that compose built-in and user-defined operations. Give each word a checked stack effect, support calls between definitions, and reject duplicate or unknown words clearly. This should ideally be done at load time. Add a 'reload' command to reload the source code for hot reloading.

## Typed entity data for small games

- [ ] Add game entities with typed fields for integers, booleans, strings, and collections. Provide operations to create entities, read and update fields, and persist the world so a small ForgeFlow game can define its rules as user-defined words.
