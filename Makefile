build:
	cargo build

install: build
	rm -f ~/bin/forgeflow && \
	cp -f target/debug/forgeflow ~/bin/forgeflow
