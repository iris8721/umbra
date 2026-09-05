.PHONY: all run run-word-count run-scheduler run-bank-account run-nqueens run-dijkstra run-json run-mandel clean

all: host

host: host.c ../umbra.h
	$(CC) $< -o $@ -I.. -L$(LIB_DIR) -lumbra -Wl,-rpath,$(LIB_DIR)

run: run-word-count

run-word-count: host
	./host word_count.umbra

run-scheduler: host
	./host scheduler.umbra

run-bank-account: host
	./host bank_account.umbra

run-nqueens: host
	./host nqueens.umbra

run-dijkstra: host
	./host dijkstra.umbra

run-json: host
	./host json.umbra

run-mandel: host
	./host mandel.umbra

clean:
	rm -f host host.exe
