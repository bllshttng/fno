// Abilities promise-tag reader plugin for openclaw.
//
// Hooks into before_agent_reply, scans the draft response for
// <promise>...</promise> tags, and writes the last tag's inner content
// to <cwd>/.fno/target-promise.signal via atomic rename.
//
// Install by symlinking this directory into ~/.openclaw/plugins/ per
// docs/SETUP-OPENCLAW.md.
//
// See docs/harnesses/promise-sentinel.md for the protocol.

import { promises as fs } from "node:fs";
import * as path from "node:path";

const PROMISE_OPEN = "<promise>";
const PROMISE_CLOSE = "</promise>";

const SENTINEL_DIR = ".fno";
const SENTINEL_FILE = "target-promise.signal";
const SENTINEL_TMP = "target-promise.signal.tmp";

type BeforeAgentReplyEvent = { cleanedBody: string };
type AgentContext = { workspaceDir?: string; cwd?: string };

async function writeSentinel(cwd: string, inner: string): Promise<void> {
  const signalDir = path.join(cwd, SENTINEL_DIR);
  await fs.mkdir(signalDir, { recursive: true });
  const tmp = path.join(signalDir, SENTINEL_TMP);
  const dst = path.join(signalDir, SENTINEL_FILE);
  const payload = `${inner}\n`;
  await fs.writeFile(tmp, payload, "utf8");
  try {
    await fs.rename(tmp, dst);
  } catch (err) {
    // Cross-device rename (EXDEV) or similar - .fno/ may be a symlink
    // across mount boundaries. Fall back to a direct write so the loop
    // wrapper still sees the sentinel.
    await fs.writeFile(dst, payload, "utf8");
    try {
      await fs.unlink(tmp);
    } catch {
      // best-effort cleanup
    }
  }
}

export default {
  name: "fno-promise-tag-reader",
  hooks: {
    before_agent_reply: async (
      event: BeforeAgentReplyEvent,
      ctx: AgentContext,
    ): Promise<void> => {
      const body = event?.cleanedBody ?? "";
      if (!body.includes(PROMISE_OPEN)) return;

      // Linear scan preserving the lazy global regex semantics the sentinel
      // protocol pins (docs/harnesses/promise-sentinel.md): each open pairs
      // with the nearest close after it, the scan resumes after that close,
      // and the last pair wins. The lazy regex itself is quadratic on
      // nested-unclosed input (CodeQL js/polynomial-redos), so it is not
      // used here.
      const inners: string[] = [];
      let at = 0;
      while (true) {
        const open = body.indexOf(PROMISE_OPEN, at);
        if (open === -1) break;
        const close = body.indexOf(PROMISE_CLOSE, open + PROMISE_OPEN.length);
        if (close === -1) break;
        inners.push(body.slice(open + PROMISE_OPEN.length, close));
        at = close + PROMISE_CLOSE.length;
      }
      if (inners.length === 0) return;

      const last = inners[inners.length - 1].trim();
      if (!last) return;

      const cwd = ctx?.workspaceDir ?? ctx?.cwd ?? process.cwd();
      try {
        await writeSentinel(cwd, last);
      } catch (err) {
        // Swallow I/O errors so a failed sentinel write does not block the
        // reply - the wrapper's stdout-scan fallback is the safety net. Log
        // to stderr so the failure is at least observable when it happens.
        console.error(
          "[fno-promise-tag-reader] sentinel write failed:",
          err,
        );
      }
    },
  },
};
