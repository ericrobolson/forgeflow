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
