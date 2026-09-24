build:
	cargo build

run:
	cargo run

test:
	cargo watch -x "test"

install: build
	rm -f ~/bin/forgeflow && \
	cp -f target/debug/forgeflow ~/bin/forgeflow
