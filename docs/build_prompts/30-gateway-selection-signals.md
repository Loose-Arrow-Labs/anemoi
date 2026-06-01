# Prompt 30: Gateway Selection Signals

## Goal

Make OpenAI-compatible gateway requests carry explicit, reviewable selection
signals into policy so repeated requests do not collapse to the same model when
their prompt shape differs.

## Issue

Closes #77.

## Scope

Allowed:

- derive conservative prompt token estimates from `messages`
- carry standard output-token limits into `InferenceRequest`
- accept explicit Anemoi metadata under an extension object
- strip the extension before forwarding to runtimes
- enforce context-window fit when request size and model capacity are known

Not allowed:

- infer task type from prompt content
- introduce prompt planning or agent memory
- change runtime execution internals

## Required Tests

Add failing tests first:

- `context_window_fit_rejects_candidate_too_small_for_request`
- `context_window_fit_allows_unknown_request_size`
- `context_window_explanation_names_required_and_available_tokens`
- `inference_gateway_derives_prompt_tokens_estimate_from_messages`
- `inference_gateway_uses_max_tokens_as_output_estimate`
- `inference_gateway_accepts_anemoi_selection_metadata`
- `inference_gateway_large_context_request_selects_larger_context_model`
- `inference_gateway_strips_anemoi_metadata_before_forwarding`

## Acceptance Criteria

- `/v1/chat/completions` builds `InferenceRequest` with prompt/output token
  estimates when the request supplies enough information.
- The gateway accepts explicit metadata in `anemoi` without forwarding that
  private extension to runtimes.
- Policy rejects candidates whose known context window is too small for the
  known request size.
- Selected-model explanations include a context-window reason when both request
  size and selected model context are known.
- A large-context gateway request can select a larger suitable model instead of
  the hot small model.

## Validation

```powershell
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p anemoi-guard -- crates
```
