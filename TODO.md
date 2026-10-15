- [x] Local models: device probe, catalog with fit/speed estimates, resumable verified download, managed llama-server
- [x] Build out a definition for a Forth like, strongly and statically typed, concatenative language for interacting with agents
- [x] Add memory (16 registers: R0-R3 durable, R4-R15 scratch)
- [x] Build out a parser to parse programs
- [x] Host-run agent loop: words exposed as tools, results in registers
- [x] Chat: each message translates to one grammar-constrained program, run deterministically
- [x] Loops: `[ ... ] each` over lists
- [x] Build out a runtime to process it
- [x] Add a project concept + initialization.
- [x] Add ability to parse + validate + load text files
- [ ] In main menu, add ability to run workflows
- [ ] Add ability to execute prompt real time, outputting stuff when you can or when user stops typing.
- [ ] Add loading of source files or scaffolding of them + durable workflow state for things like agentic pipelines
- [ ] Add swap/swizzle/etc (dup, drop, swap done)
- [ ] Add function definitions
- [x] File editor with a cached, line-anchored pseudocode summary beside the text (`forgeflow edit`, `/edit`, `summarize`)
- [ ] Add a visualizer/editor so as you move your cursor over a word it'll show the stack and any errors visually
- [ ] Conditionals
- [ ] Run `each` items concurrently on spare server slots
- [ ] Smaller translator model option, separate from the answer model
- [ ] Anthropic Messages backend
- [ ] CLI backends (claude/codex/opencode) behind the loop, with tool calls as text
- [ ] Text tool-call format for models without native tool calling
- [ ] Write queue words (queue_write, pending, approve, apply) ported from flowforge
- [ ] Archive words (find_symbols, show_symbol, reindex) ported from flowforge
- [ ] `bench` word: measure real tokens/s and replace the estimate
- [ ] Batch compaction of old `reg_view` results when the context fills


## User-defined words

- [x] Add Forth-style word definitions in recursively discovered `_forgeflow/**/*.ff` files and `_forgeflow/user_words.ff.txt`, with checked named stack effects, cross-file calls, tail-call recursion, durable named address cells, agent tools, and `/reload`.
