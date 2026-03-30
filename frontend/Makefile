# Makefile for Elm frontend build
# Regenerates the static/ directory with compiled Elm code

# Variables
ELM = elm
SRC_DIR = src
STATIC_DIR = static
MAIN_ELM = $(SRC_DIR)/Main.elm
OUTPUT_JS = $(STATIC_DIR)/main.js

# Source files (all .elm files trigger rebuild)
ELM_SOURCES = $(shell find $(SRC_DIR) -name '*.elm')

# Static assets that should exist
STATIC_ASSETS = $(STATIC_DIR)/index.html \
                $(STATIC_DIR)/ports.js \
                $(STATIC_DIR)/styles/custom.css

# Default target
all: build

# Development build
build: $(OUTPUT_JS)

$(OUTPUT_JS): $(ELM_SOURCES)
	$(ELM) make $(MAIN_ELM) --output=$(OUTPUT_JS)

# Production build with optimization
build-prod: $(ELM_SOURCES)
	$(ELM) make $(MAIN_ELM) --output=$(OUTPUT_JS) --optimize

# Clean generated files
clean:
	rm -f $(OUTPUT_JS)
	rm -rf elm-stuff/

# Phony targets
.PHONY: all build build-prod clean
