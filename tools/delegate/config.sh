# Delegated-execution config. THIS IS THE ONLY FILE TO EDIT when changing models.
#
# The executor and the delegate tier must be co-resident, or every delegation
# evicts the executor and costs a full cold reload. llama-swap declares legal
# pairs in its `matrix.sets.co-located` rule; being legal there is NOT the same
# as fitting in VRAM (see anemoi #188). Verify a new pair with:
#   tools/delegate/delegate check-pair

# --- Executor: the model that does the work ---
EXECUTOR_PROVIDER="${EXECUTOR_PROVIDER:-prometheus-llama-swap}"
EXECUTOR_MODEL="${EXECUTOR_MODEL:-qwen3.8-27b-co}"

# --- Delegate tier: the cheap model the executor farms subtasks to ---
# NOTE: lfm2-5-350m-gpu fits comfortably but is too weak for real subtasks.
# qwen3.5-2b-mtp-co is the wanted tier; it currently misses by ~50 MiB against a
# 27B at ctx 70000. Closing that gap is anemoi #188 (trim 27B ctx to ~50000, or
# drop the 2B to --parallel 1).
DELEGATE_MODEL="${DELEGATE_MODEL:-lfm2-5-350m-gpu}"
DELEGATE_BASE_URL="${DELEGATE_BASE_URL:-http://llama-swap.home.arpa/v1}"
# Auth token for the runtime. Not committed; export it or set it in your shell profile.
DELEGATE_TOKEN="${DELEGATE_TOKEN:-${ANEMOI_LLAMA_SWAP_AUTH_TOKEN:-}}"

# --- Backend (for residency checks) ---
LLAMA_SWAP_URL="${LLAMA_SWAP_URL:-http://llama-swap.home.arpa}"

# --- Tuning ---
DELEGATE_MAX_TOKENS="${DELEGATE_MAX_TOKENS:-2048}"
DELEGATE_TIMEOUT_MS="${DELEGATE_TIMEOUT_MS:-120000}"
DELEGATE_MAX_PROMPT_CHARS="${DELEGATE_MAX_PROMPT_CHARS:-8000}"

# Where run folders are created.
RUNS_ROOT="${RUNS_ROOT:-$HOME/.anemoi/runs}"
