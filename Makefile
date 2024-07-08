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
	cd tests; poetry run pytest . -s -x

clean:
	$(CC) clean

clippy:
	$(CC) clippy --all --tests

release:
	$(CC) build --release
