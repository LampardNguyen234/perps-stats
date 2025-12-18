.PHONY: help build check fmt test clean start serve docker-build docker-start docker-stop docker-logs

# Default target
help:
	@echo "Perps Stats - Available Commands"
	@echo "=================================="
	@echo ""
	@echo "Build & Development:"
	@echo "  make build          - Build release binary"
	@echo "  make check          - Check code without building"
	@echo "  make fmt            - Format code"
	@echo "  make clippy         - Run Clippy linter"
	@echo "  make test           - Run tests"
	@echo "  make clean          - Clean build artifacts"
	@echo ""
	@echo "Running:"
	@echo "  make start          - Start unified service (data collection + API)"
	@echo "  make serve          - Start API server only"
	@echo ""
	@echo "Docker (with docker-compose):"
	@echo "  make docker-build   - Build Docker image"
	@echo "  make docker-start   - Start Docker services (perps-stats + PostgreSQL)"
	@echo "  make docker-stop    - Stop Docker services"
	@echo "  make docker-ps      - Show Docker service status"
	@echo "  make docker-logs    - View Docker service logs"
	@echo "  make docker-clean   - Remove Docker containers and volumes"
	@echo ""
	@echo "Database:"
	@echo "  make db-migrate     - Run database migrations"
	@echo "  make db-stats       - Show database statistics"
	@echo ""

# Build commands
build:
	@echo "Building release binary..."
	cargo build --release
	@echo "✓ Build complete. Binary at: target/release/perps-stats"

check:
	@echo "Checking code..."
	cargo check

fmt:
	@echo "Formatting code..."
	cargo fmt

clippy:
	@echo "Running Clippy linter..."
	cargo clippy --all-targets --all-features

test:
	@echo "Running tests..."
	cargo test

clean:
	@echo "Cleaning build artifacts..."
	cargo clean

# Running the service
start:
	@echo "Starting unified service (data collection + API)..."
	@echo "Prerequisites: DATABASE_URL must be set"
	@echo "Prerequisites: symbols must be set in symbols.txt"
	@echo "Prerequisites: exchanges must be set in exchanges.txt"
	@echo "Make sure symbols.txt exists in the project root"
	@echo ""
	cargo run --release -- start \
		--symbols-file symbols.txt \
		-e $$(cat exchanges.txt) \
		--enable-api \
		--api-port 9999 \
		--pool-size 100

serve:
	@echo "Starting API server only..."
	@echo "Prerequisites: DATABASE_URL must be set and database must have data"
	@echo ""
	cargo run --release -- serve --port 9999

# Docker commands
docker-build:
	@echo "Building Docker image..."
	docker build -t perps-stats:latest .
	@echo "✓ Docker image built: perps-stats:latest"

docker-start:
	@echo "Starting Docker container..."
	@echo "Prerequisites: DATABASE_URL must be set"
	@echo "Prerequisites: symbols.txt and exchanges.txt must exist"
	@echo ""
	make docker-build && docker run -d \
		--name perps-stats \
		-v $(PWD)/symbols.txt:/app/symbols.txt:ro \
		-v $(PWD)/exchanges.txt:/app/exchanges.txt:ro \
		-e DATABASE_URL=$(DATABASE_URL) \
		-p 9999:9999 \
		perps-stats:latest
	@echo "✓ Container started. API available at http://127.0.0.1:9999/api/"
	@echo "View logs with: make docker-logs"

docker-stop:
	@echo "Stopping Docker container..."
	docker stop perps-stats 2>/dev/null || echo "Container not running"
	docker rm perps-stats 2>/dev/null || echo "Container not found"
	@echo "✓ Container stopped"

docker-logs:
	@echo "Showing Docker container logs..."
	docker logs -f perps-stats

# Database commands
db-migrate:
	@echo "Running database migrations..."
	@echo "Prerequisites: DATABASE_URL must be set"
	@echo ""
	cargo run -- db migrate

db-stats:
	@echo "Showing database statistics..."
	@echo "Prerequisites: DATABASE_URL must be set"
	@echo ""
	cargo run -- db stats
