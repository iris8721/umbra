LIB_DIR = ../target/debug

.PHONY: all run run-stdlib run-coroutine run-oop clean

all: host

host: host.c ../umbra.h
	$(CC) $< -o $@ -I.. -L$(LIB_DIR) -lumbra -Wl,-rpath,$(LIB_DIR)

run: run-stdlib

run-stdlib: host
	./host stdlib_demo.umbra

run-coroutine: host
	./host coroutine_demo.umbra

run-oop: host
	./host oop_demo.umbra

clean:
	rm -f host host.exe
