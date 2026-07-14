# Sauron developer shortcuts.
.PHONY: help up down logs build dev-infra migrate api ingest test fmt clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

up: ## Build & start the whole stack (docker compose)
	docker compose up --build

down: ## Stop the stack
	docker compose down

logs: ## Tail service logs
	docker compose logs -f api ingest

build: ## Compile the Rust workspace
	cd backend && cargo build --workspace

test: ## Run the Rust test suite
	cd backend && cargo test --workspace

fmt: ## Format the Rust workspace
	cd backend && cargo fmt

# --- local development without full compose ---

dev-infra: ## Start only Postgres + Redis for local dev
	docker compose up -d postgres redis

migrate: ## Apply DB migrations (expects DATABASE_URL)
	cd backend && cargo run -p sauron-migrate

api: ## Run the dashboard API locally (expects DATABASE_URL, REDIS_URL, JWT_SECRET)
	cd backend && cargo run -p sauron-api

ingest: ## Run the ingest gateway locally
	cd backend && cargo run -p sauron-ingest

clean: ## Remove build artifacts and volumes
	cd backend && cargo clean
	docker compose down -v
