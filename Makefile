all: 
	@echo "Building binaries"
	cargo build --release
	mv target/release/tcp_node ./node

clean: 
	rm ./node
	rm -r target/

