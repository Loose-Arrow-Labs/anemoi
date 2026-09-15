import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { appendFileSync } from "node:fs";

const BASE_URL = process.env.ANEMOI_DELEGATE_BASE_URL ?? "http://llama-swap.home.arpa/v1";
const MODEL = process.env.ANEMOI_DELEGATE_MODEL ?? "lfm2-5-350m-gpu";
const TOKEN = process.env.ANEMOI_DELEGATE_TOKEN ?? "LOCAL";
const MAX_TOKENS = Number(process.env.ANEMOI_DELEGATE_MAX_TOKENS ?? 2048);
const TIMEOUT_MS = Number(process.env.ANEMOI_DELEGATE_TIMEOUT_MS ?? 120_000);
const MAX_PROMPT_CHARS = Number(process.env.ANEMOI_DELEGATE_MAX_PROMPT_CHARS ?? 8000);
const LOG_PATH = process.env.ANEMOI_DELEGATE_LOG;

type Delegation = {
  ts: string;
  model: string;
  ok: boolean;
  ms: number;
  promptChars: number;
  replyChars: number;
  error?: string;
  anemoi?: Record<string, string>;
};

// Delegations are logged to a file rather than stdout because the orchestrator
// polls artifacts, not the transcript, and an empty pi.log has already been
// mistaken for an idle worker once.
function record(entry: Delegation): void {
  if (!LOG_PATH) return;
  try {
    appendFileSync(LOG_PATH, `${JSON.stringify(entry)}\n`, "utf8");
  } catch {
    // Never fail a delegation because bookkeeping failed.
  }
}

function anemoiHeaders(response: Response): Record<string, string> | undefined {
  const out: Record<string, string> = {};
  for (const key of ["x-anemoi-decision-id", "x-anemoi-selected-model", "x-anemoi-action"]) {
    const value = response.headers.get(key);
    if (value) out[key] = value;
  }
  return Object.keys(out).length > 0 ? out : undefined;
}

export default function (pi: ExtensionAPI) {
  pi.registerTool({
    name: "delegate_subtask",
    label: "Delegate Subtask",
    description:
      "Send a small, self-contained subtask to a fast co-resident model and return its answer. " +
      "Use it for mechanical work where a large model is wasteful: reformatting, extracting a list " +
      "of names or symbols from text you paste in, summarizing a stack trace, classifying a string, " +
      "or drafting a short docstring. The target is a very small model: it cannot see your files, " +
      "your conversation, or the repository, so the prompt must be fully self-contained and include " +
      "any text it needs to work on. Do not use it for design decisions, multi-step reasoning, " +
      "writing code that must compile, or anything where being wrong is expensive - it is faster " +
      "but substantially less capable than you. Always sanity-check what it returns.",
    promptSnippet:
      "Delegate a small self-contained subtask to a fast, less capable co-resident model",
    promptGuidelines: [
      "Use delegate_subtask for cheap mechanical text work, and verify its output before relying on it.",
      "delegate_subtask cannot read files or see context - inline everything it needs into the prompt.",
    ],
    parameters: Type.Object({
      prompt: Type.String({
        description:
          "The complete, self-contained instruction plus any text to operate on. The target model " +
          "has no other context.",
      }),
      system: Type.Optional(
        Type.String({
          description: "Optional system instruction, e.g. 'Reply with only a JSON array of strings.'",
        }),
      ),
    }),

    async execute(_toolCallId, params, signal, onUpdate) {
      const started = Date.now();
      const promptChars = params.prompt.length;

      if (promptChars > MAX_PROMPT_CHARS) {
        const error = `prompt is ${promptChars} chars, over the ${MAX_PROMPT_CHARS} limit for the small tier; shrink it or handle this yourself`;
        record({
          ts: new Date().toISOString(),
          model: MODEL,
          ok: false,
          ms: 0,
          promptChars,
          replyChars: 0,
          error,
        });
        return { content: [{ type: "text", text: `delegate_subtask refused: ${error}` }], isError: true };
      }

      onUpdate?.({ content: [{ type: "text", text: `Delegating to ${MODEL}...` }] });

      const messages = [
        ...(params.system ? [{ role: "system", content: params.system }] : []),
        { role: "user", content: params.prompt },
      ];

      // The tool's own timeout is separate from the agent abort signal: a cold
      // model load can outlast a client's patience without the agent aborting.
      const timer = new AbortController();
      const timeout = setTimeout(() => timer.abort(), TIMEOUT_MS);
      const onAbort = () => timer.abort();
      signal?.addEventListener("abort", onAbort, { once: true });

      try {
        const response = await fetch(`${BASE_URL}/chat/completions`, {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            Authorization: `Bearer ${TOKEN}`,
          },
          body: JSON.stringify({ model: MODEL, messages, max_tokens: MAX_TOKENS }),
          signal: timer.signal,
        });

        const anemoi = anemoiHeaders(response);

        if (!response.ok) {
          const body = (await response.text()).slice(0, 400);
          const error = `HTTP ${response.status}: ${body}`;
          record({
            ts: new Date().toISOString(),
            model: MODEL,
            ok: false,
            ms: Date.now() - started,
            promptChars,
            replyChars: 0,
            error,
            anemoi,
          });
          return {
            content: [
              {
                type: "text",
                text:
                  `delegate_subtask failed: ${error}\n` +
                  "The subtask was NOT completed. Do the work yourself rather than assuming a result.",
              },
            ],
            isError: true,
          };
        }

        const payload = (await response.json()) as {
          choices?: Array<{ message?: { content?: string | null }; finish_reason?: string }>;
        };
        const choice = payload.choices?.[0];
        const reply = (choice?.message?.content ?? "").trim();

        // A reasoning-enabled target can spend its whole allowance on hidden
        // reasoning and return empty content with finish_reason "length".
        if (reply.length === 0) {
          const error = `empty reply (finish_reason=${choice?.finish_reason ?? "unknown"})`;
          record({
            ts: new Date().toISOString(),
            model: MODEL,
            ok: false,
            ms: Date.now() - started,
            promptChars,
            replyChars: 0,
            error,
            anemoi,
          });
          return {
            content: [
              {
                type: "text",
                text:
                  `delegate_subtask returned nothing (${error}). ` +
                  "Treat the subtask as not done; either retry with a simpler prompt or handle it yourself.",
              },
            ],
            isError: true,
          };
        }

        record({
          ts: new Date().toISOString(),
          model: MODEL,
          ok: true,
          ms: Date.now() - started,
          promptChars,
          replyChars: reply.length,
          anemoi,
        });

        return {
          content: [{ type: "text", text: reply }],
          details: { model: MODEL, ms: Date.now() - started, anemoi },
        };
      } catch (cause) {
        const aborted = timer.signal.aborted && !signal?.aborted;
        const error = aborted
          ? `timed out after ${TIMEOUT_MS}ms (model may be cold-loading or evicted)`
          : String(cause);
        record({
          ts: new Date().toISOString(),
          model: MODEL,
          ok: false,
          ms: Date.now() - started,
          promptChars,
          replyChars: 0,
          error,
        });
        return {
          content: [
            {
              type: "text",
              text: `delegate_subtask failed: ${error}\nThe subtask was NOT completed; handle it yourself.`,
            },
          ],
          isError: true,
        };
      } finally {
        clearTimeout(timeout);
        signal?.removeEventListener("abort", onAbort);
      }
    },
  });
}
