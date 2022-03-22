all: 
	@echo "Building binaries"
	cargo build --release
	mv target/release/node ./

clean: 
	rm ./node
	rm -r target/

