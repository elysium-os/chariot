.PHONY: all clean

all: clean chariot

chariot:
	gcc -std=gnu23 -D_GNU_SOURCE $(shell find ./src -type f -name "*c") -o $@

clean:
	rm -f chariot
