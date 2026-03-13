CARGO = /c/Users/Alexey/.cargo/bin/cargo.exe
TARGET = x86_64-pc-windows-msvc
RELEASE_BIN = target/$(TARGET)/release/microboost.exe

.PHONY: build run kill clean folder open rebuild

build:
	@$(CARGO) build --release --target $(TARGET)

run: build
	@./$(RELEASE_BIN) &

kill:
	@taskkill /f /im microboost.exe 2>/dev/null || true

clean:
	@$(CARGO) clean

folder:
	@explorer "%APPDATA%\Microboost"

open:
	@cmd /c start $(subst /,\\,$(RELEASE_BIN))

rebuild: kill build
	@echo "Done. Run: make open"
