CC = gcc
CFLAGS := -O2
all: build test
build: main.o
	$(CC) -o app main.o
test:
	./run-tests
define HELP
usage
endef
.PHONY: all build test
