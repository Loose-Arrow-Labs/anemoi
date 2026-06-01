---
name: Bug report
about: A reproducible bug report
title: "[BUG]<scope>: <short title>"
labels: bug
assignees: alucero270

---

## Summary
What is broken? (one sentence: what broke and where)

## Steps to Reproduce
1.
2.
3.

## Expected Behavior
What should happen?

## Actual Behavior
What happens now? Include logs/errors.

## Environment
- OS:
- Rust toolchain (`rustup show`):
- Anemoi config (`ANEMOI_CONFIG`):
- Runtime adapter (llama-swap / Ollama / mock):
- `ANEMOI_ENABLE_LIVE_EXECUTE` set: yes / no

## Relevant Output

<!--
Paste cargo test output, daemon logs, or CLI output. Redact tokens, hostnames, and private paths.
-->

```
```

## Scope
Crates/areas likely involved.

## Acceptance Criteria
- [ ] Bug fixed and repro no longer fails
- [ ] CI passes (`fmt` + `clippy` + `test` jobs green)
- [ ] Regression test added (if appropriate)
- [ ] Docs updated if behavior changed

## Notes for Codex
Any hints, constraints, or "do not touch" areas.
