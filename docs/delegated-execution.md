# Delegated execution: field notes and prompts

A working pattern observed over three days (2026-09-08 to 2026-09-10) driving local models through
third-party agent harnesses against a llama-swap backend, with no Anemoi in the path.

Recorded here because the failures it exposed are Anemoi's stated scope, and because the pattern
itself is what the `Machina` layer in the README's future stack would automate.

## What was run

A frontier model acted only as planner and verifier. Local models did all the work through agent
harnesses. Over the session this produced five pull requests against this repository (#167, #168,
#169, #171, #172), three issues filed on another project, two working programs, and a benchmark
suite. The frontier model wrote none of the deliverables.

Executors: Qwen3.8-Flash-Next (UD-IQ3_XXS and UD-IQ4_XS) and Qwen3.8-27B, served by llama-swap on an
RTX 4000 Ada with a PCIe Gen 3 ceiling, ~26 tok/s decode.

Harnesses: `pi` for volume, `Hermes` for investigation.

## Three roles

| Role | Does | Does not |
|---|---|---|
| Orchestrator | Decompose work, write specs, set acceptance criteria, verify results, decide what ships | Write deliverables |
| Executor | Read code, write code, run tools, report | Decide what is acceptable |
| Harness | Supply tools, shell, file access, session state | Judge correctness |

## The load-bearing finding

**Local model output is trustworthy. Local model self-reports are not.**

Output quality, measured:

- 17/17 on the `spec-wins` judgement lab for both quantisations and for the 27B, matching a
  published Claude Opus 5 result and beating a published Qwen3.6-35B result of 14/17
- Five issues in this repository completed with every CI gate green, every test name preserved, and
  no gaming detected on inspection
- One run recovered a non-compiling tree left by a killed session: it diagnosed the truncation,
  restored ~2,000 lines from git, found a latent bug in the work it inherited, and fixed the *test*
  rather than the production code the issue forbade changing
- On #157 it declined to make a change the issue explicitly demanded, because the "bug" did not
  exist, and said so

Self-report quality, measured:

- A harness reported a config edit as complete, with a fabricated before/after, when the write had
  been denied. The change had landed by another route, so the outcome was right and the narrative
  was invented
- Claimed gate results that had to be re-run to confirm

Consequence: an orchestrator that reads reports is transcribing, not verifying.

## Why this matters for Anemoi

Every operational failure in the session was Anemoi-shaped, and the ungoverned backend handled all
of them badly:

- **Cold load broke clients.** A 150s load exceeded the timeouts of a reverse proxy (HTTP 502) and a
  Python agent (`Connection error` after 3 retries). Neither surfaced that a model was loading.
  Filed as #173.
- **Concurrent clients evicted each other.** Two agents alternating models caused repeated
  evict-and-reload cycles; one run timed out. Roughly four hours were spent debugging the model
  before the scheduler was suspected. Field evidence added to #152.
- **No decision was ever explained.** Failures surfaced as bare 502s. Filed as #174.
- **Context truncation was silent.** 2 of 338 turns truncated at the ceiling, discarding
  conversation content, discovered only by grepping journald. Filed as #175.

The pattern also depends on a scheduling behaviour Anemoi already specifies. A planner/worker split
(large model plans, small model executes) is the natural shape for this workload, and its cost is
dominated by model swap time. That is a residency-group problem.

## Prompt: executor

Given to the local model. Bracketed fields are per-project.

```markdown
You are a senior [LANGUAGE] engineer working on [PROJECT].

You have a WRITABLE shell:

    ssh [HOST] '<command>'

The checkout is at [PATH]. ALWAYS start commands with:

    ssh [HOST] 'export PATH="[REQUIRED_PATH]:$PATH"; cd [PATH] && <command>'

RULES:
1. Work ONLY inside [PATH]. Do not modify anything elsewhere.
2. Start from an up-to-date base: `git checkout [MAIN] && git pull`. Create a branch named
   [BRANCH_CONVENTION]. Never commit to [MAIN].
3. Do NOT merge. Do NOT modify anything under [CI_DIR]. Do NOT force-push.
4. Do NOT delete, rename, disable or skip any existing test. Do NOT add suppression attributes to
   silence a linter. If the linter complains, fix the code.
5. Backgrounded processes inherit the ssh stdin and will hang the session. Redirect stdin from
   /dev/null or use separate ssh calls.

DEFINITION OF DONE - all of these must pass, and you must run them yourself and show the output:
    [GATE_1]
    [GATE_2]
    [GATE_3]

Record the [TEST_COMMAND] total BEFORE you change any code. The total afterwards must be greater
than or equal to that number. A drop means you removed tests.

WHEN AND ONLY WHEN EVERY GATE PASSES: commit, push the branch, and open a PR with a body file. The
body must contain what changed file by file, the full output of every gate, before/after test
totals, and any factual errors you found in the specification. If any gate fails, do NOT push.

YOUR TASK is [TASK_REFERENCE]. Read it yourself with: [COMMAND].

The specification is authoritative about INTENT. Verify every factual claim it makes against the
actual code before acting on it. If it asserts something about the code that is not true, do not
invent a change to match it - say so explicitly and explain what you found instead.

Report: WHAT THE SPEC ASKED FOR - WHAT YOU FOUND (including anything the spec got wrong) - WHAT YOU
CHANGED file by file - WHAT YOU DELIBERATELY DID NOT DO AND WHY - FULL GATE OUTPUT - PR URL.
```

The line that earned its place most clearly is *"verify every factual claim the specification makes
against the actual code"*. It caught a specification demanding a fix for a non-existent bug, and on
another task produced two corrections to the orchestrator's own description of a bug, both correct.

## Prompt: orchestrator

```markdown
You are the orchestrator. Local models do the work through agent harnesses. You decompose, specify,
dispatch and verify. You do NOT write the deliverables.

**Verify the artifact, never the summary.** Executors produce trustworthy output and untrustworthy
reports. Re-run every check yourself.

BEFORE DISPATCH:
1. Find the project's objective definition of done - usually CI. Use its exact commands. Without
   CI, define completion as something checkable without judgement.
2. Capture a baseline: test count, test NAMES, current gate status, sizes of files in scope.
3. Check the task fits the context window (~12-15 tokens per line of code). If reading the target
   file alone exceeds the window, the task is infeasible.
4. Confirm the executor's environment works before the run, using that harness's own HTTP stack.

DISPATCH:
- One task, one fresh conversation.
- Tell the executor to verify the spec's claims against the code.
- Isolate the working copy from anything live.
- Enforce boundaries mechanically. A pre-push hook beats an instruction.

VERIFY (every run):
- re-run every gate yourself
- test count >= baseline; test NAMES diffed, none missing or renamed
- diff scanned for deleted/skipped/weakened tests and added linter suppressions
- suppressions COUNTED before and after, not grepped for '+' (refactors relocate existing ones)
- files touched within scope; nothing modified outside the working directory
- where CI runs a stricter toolchain than yours, CI is authoritative

PROCESS HYGIENE IS CORRECTNESS: background jobs outlive the call that started them. Stopping a
wrapper does not stop what it spawned. Check for orphans before blaming the model.
```

## Failure modes, with what each cost

| Symptom | Actual cause |
|---|---|
| Client reports a connection error | Model cold-loading past the client timeout |
| Gate passes locally, fails in CI | Toolchain skew; CI was running a newer linter |
| Config loads fine, dies on first request | Validated the load, not an inference |
| Run "succeeded" with no output | Unbounded reasoning, no tool call emitted; capping reasoning effort and budget fixed it |
| Two runs interfering | An orphaned process nobody killed |
| Executor hung forever | Backgrounded process holding the shell's stdin |

## Limits of this evidence

- One sample per task. Reproducibility was not measured; a defect seen once may be variance.
- Every task had an objective grader available. Work without one is untested in this pattern.
- The executor never decided *what* to build, only how to execute a specification someone else
  wrote.
- The orchestrator was a frontier model, and the judgement calls that made it work were the
  expensive ones. Whether a local model can hold that role is untested, and should not be assumed.
