.PHONY: build run kill clean folder open

build:
	@export PATH="/c/Users/alexe/.cargo/bin:$$PATH" && cargo build --release

run: build
	@./target/release/microboost.exe &

kill:
	@taskkill /f /im microboost.exe 2>/dev/null || true

clean:
	@cargo clean

folder:
	@explorer "%APPDATA%\Microboost"

open:
	@cmd /c start target\\release\\microboost.exe

rebuild: kill build
	@echo "Done. Run: make open"
