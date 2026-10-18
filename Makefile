CC=cargo
FMT=fmt

OPTIONS=

default: fmt
	$(CC) build
	@make clippy

fmt:
	$(CC) fmt --all

check:
	$(CC) build
	$(CC) test --all
	# RUST=0: our lightningd is built without Rust, so pyln must not pass --grpc-port.
	cd tests; RUST=0 poetry run pytest . -s -x

clean:
	$(CC) clean

clippy:
	$(CC) clippy --all --tests

release:
	$(CC) build --release

# Generate a strategy skeleton: make strategy NAME=my_strategy LANG=rust|python
strategy:
	python3 contrib/new_strategy.py $(NAME) --lang $(or $(LANG),rust)
