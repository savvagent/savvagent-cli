# savvagent-development — Agent Dispatch Prompt Templates

Verbatim prompt bodies for every `task`-tool dispatch in `SKILL.md`. The skill spine owns the
*decision* logic (when to dispatch, which `agent_type` / `model` / `reasoning_effort`, status
handling, fix-loop caps, convergence rules); this file owns the prompt *text* that goes in the
`prompt` field.

**Use the template exactly. Do not improvise a dispatch prompt body.** Fill the `<...>` placeholders
from your working memory (spec / plan / task text / report verbatim, plus the relevant Load-Bearing
Invariants and Repository Conventions from `SKILL.md`). The `agent_type` and `mode` are named in
each template's heading — honor them; see "How dispatch works in this environment" in `SKILL.md` for
the mapping from this skill's role names to this CLI's actual `agent_type`s. **Note:** the `task`
tool and `agent_type`/`mode` fields referenced throughout this file belong to the *orchestrating*
GitHub Copilot CLI running this skill — a different thing from savvagent-cli's own in-product `task`
tool (`README.md`, `{ description, prompt, subagent_type }`), which is a runtime feature of the
software being built, not the dispatch mechanism used to build it.

`<ref>` below is the source reference: a GitHub issue (`savvagent/savvagent-cli#123`) or — on the
ticketless path — the captured task brief.

Every dispatch below is a single `task` tool call: `agent_type`, `mode`, `name` (short display name),
`description` (3–5 words), and `prompt` (the full body). Fill `name`/`description` yourselves; only
the `prompt` body is templated here.

---

## Spec Critique — Phase 1 Step 4 — `agent_type: "rubber-duck"` (fallback `"general-purpose"`), `mode: "sync"`

```
task tool:
  agent_type: rubber-duck
  mode: sync
  description: "Review spec document"
  prompt: |
    You are a spec document reviewer. Verify this spec is complete and ready for planning.

    Spec to review (full text inline; it is committed at docs/superpowers/specs/<file>):
    <PASTE FULL SPEC TEXT>

    Repo profile — the invariants and conventions this spec must respect:
    <PASTE the "Load-Bearing Invariants" section and the relevant Repository Conventions rows from SKILL.md>

    Check:
    - Completeness: TODOs, placeholders, "TBD", incomplete sections
    - Consistency: internal contradictions, conflicting requirements
    - Clarity: requirements ambiguous enough to cause someone to build the wrong thing
    - Scope: focused enough for a single plan, with explicit non-goals
    - The everything-is-MCP-shaped architecture: does the change respect the host/tool/provider
      crate boundaries, or does it try to put turn-loop logic in a tool, or provider-selection logic
      in a tool, or an HTTP/registry concept inside Host?
    - The host-swap RwLock rule: if the change touches crates/savvagent/src/app.rs or tui.rs, does
      the spec call out that no .await may execute while the Arc<RwLock<...>> read guard is held?
    - The rmcp ProgressDispatcher gotcha: if the change adds/touches a streaming provider path, does
      the spec call out the forwarder-abort requirement?
    - Public-interface changes: is any non-additive change to the SPP wire format, a tool's MCP
      schema, the plugin ABI, a slash command, an env var, or the on-disk transcript/keyring format
      named explicitly? (Non-Negotiable Rule 6.)
    - YAGNI: unrequested features, over-engineering
    - Alignment with the source AC (cite ref <ref>)

    Only flag issues that would cause real problems during planning. Approve unless there are serious gaps.

    Output:
    ## Spec Review
    Status: Approved | Issues Found
    Issues: - [Section X]: [issue] - [why it matters]
    Recommendations (advisory): - [...]
```

---

## Plan Critique — Phase 2 Step 6 — `agent_type: "rubber-duck"` (fallback `"general-purpose"`), `mode: "sync"`

```
task tool:
  agent_type: rubber-duck
  mode: sync
  description: "Review plan document"
  prompt: |
    You are a plan document reviewer. Verify this plan is complete and ready for implementation.

    Plan to review (full text inline; it is committed at docs/superpowers/plans/<file>):
    <PASTE FULL PLAN TEXT>

    Spec for reference (full text inline):
    <PASTE FULL SPEC TEXT>

    Repo profile — invariants, conventions, and commands:
    <PASTE the "Load-Bearing Invariants" section, the Repository Conventions table, and the test/lint commands from SKILL.md>

    Check completeness, spec alignment, task decomposition, buildability, and whether each task
    respects the repo-specific requirements above. In particular:
    - Does every task use the repo's real commands (cargo test -p <crate>, cargo test -p <crate> --
      <test name>, cargo clippy --workspace --all-targets, cargo fmt --all)?
    - Does a task touching crates/savvagent/src/app.rs or tui.rs include an explicit step verifying
      the host-swap RwLock rule (no .await under the read guard)?
    - Does a task adding/modifying a streaming provider path include a step preserving the
      ProgressDispatcher forwarder-abort pattern?
    - Does any task try to give Host a provider registry, or put turn-loop logic outside
      savvagent-host?
    - Does a task changing a public interface (SPP wire format, tool schema, plugin ABI, slash
      command, env var, on-disk format) call out whether it's additive or breaking, per
      Non-Negotiable Rule 6?
    - Does the final task bump workspace.package.version + workspace.dependencies versions in
      Cargo.toml and add the CHANGELOG.md entry, per this skill's release requirement?

    Only flag issues that would cause an implementer to build the wrong thing or get stuck.

    Output:
    ## Plan Review
    Status: Approved | Issues Found
    Issues: - [Task X, Step Y]: [issue] - [why it matters]
    Recommendations (advisory): - [...]
```

---

## Implementer Dispatch — Phase 3 Step A — `agent_type: "general-purpose"`, `mode: "sync"`

```
task tool:
  agent_type: general-purpose
  mode: sync
  description: "Implement Task N: <name>"
  prompt: |
    You are implementing Task N: <name>

    ## Task Description
    <FULL TEXT of the task pasted inline — do not reference the plan file>

    ## Context
    <2-4 sentences: where this fits, dependencies on prior tasks, architectural notes, plus the
    repo-specific reminders for this task — the applicable Load-Bearing Invariants, the test/lint
    commands, and any public-interface change this task makes>

    Run `cargo fmt --all` before every Rust commit. Tests are self-contained (no external services
    or database needed). Never add AI self-attribution to anything. If this task
    touches crates/savvagent/src/app.rs or tui.rs, never hold the Arc<RwLock<Option<Arc<Host>>>> read
    guard across an .await. If this task adds/touches a streaming provider path, abort the
    ProgressDispatcher forwarder task after the request future resolves.

    ## AUTONOMOUS MODE — IMPORTANT

    You are running inside an autonomous pipeline. Do NOT ask clarifying questions.
    There is no developer available to answer mid-run.

    Instead:
    - When the task is ambiguous, pick the most reasonable interpretation given the
      surrounding code and the spec. Document the assumption in your report.
    - If the assumption is high-risk (could plausibly be wrong in a way the developer
      would care about), report DONE_WITH_CONCERNS and list the assumption explicitly.
    - Only return BLOCKED if you genuinely cannot proceed without information that
      cannot be reasonably inferred (e.g., a missing credential, an undocumented external
      contract). Do NOT return BLOCKED for stylistic ambiguity.

    ## Your Job
    1. Follow the task's TDD steps in order: failing test → run → implement → run → commit.
    2. Use exact file paths and commands from the task. Do not invent your own.
    3. Self-review before reporting (completeness, quality, YAGNI, testing).
    4. Commit per the task's step-by-step instructions, using the repo's `<scope>: <subject>` format.

    Work from: <worktree absolute path>

    ## Report Format
    - Status: DONE | DONE_WITH_CONCERNS | BLOCKED | NEEDS_CONTEXT
    - Files changed (with commit SHAs)
    - Test results (command + outcome)
    - Assumptions made (with one-line rationale each)
    - Concerns or blockers (if any)
```

---

## Spec Compliance Review — Phase 3 Step C — `agent_type: "general-purpose"`, `mode: "sync"`

```
task tool:
  agent_type: general-purpose
  mode: sync
  description: "Spec compliance: Task N"
  prompt: |
    You are reviewing whether an implementation matches its specification.

    ## What Was Requested
    <FULL TEXT of the task — same as the implementer received>

    ## What Implementer Claims They Built
    <implementer's report verbatim>

    ## CRITICAL: Do Not Trust The Report
    Read the actual code at the commit SHAs they listed. Verify line-by-line.

    Check:
    - Missing requirements (claimed implemented but actually skipped)
    - Extra work (built features not requested)
    - Misinterpretations (right feature, wrong way)
    - Repo-specific gotchas: an Arc<RwLock<...>> read guard held across an .await, a
      ProgressDispatcher forwarder task never aborted, a provider registry sneaking into Host, an
      unwrap() outside tests, a tool wired without going through ToolRegistry's stdio transport, a
      secret written to disk in plaintext (transcript JSON, logs, error messages) instead of the OS
      keyring, a public-interface change (SPP wire format, tool schema, plugin ABI, slash command)
      not flagged as additive/breaking.

    Report:
    - ✅ Spec compliant
    - ❌ Issues found: [list with file:line refs]
```

---

## Code Quality Review — Phase 3 Step E — `agent_type: "code-review"`, `mode: "sync"`

Capture commit boundaries first (SKILL.md step E owns this):
`BASE_SHA = git rev-parse HEAD~<N>` (N = commits this task produced), `HEAD_SHA = git rev-parse HEAD`.

```
task tool:
  agent_type: code-review
  mode: sync
  description: "Quality: Task N"
  prompt: |
    Review the code changes between <BASE_SHA> and <HEAD_SHA>.

    Plan/requirements: Task N (full text inline):
    <FULL TEXT of task>

    Check standard code-quality concerns plus:
    - One clear responsibility per file?
    - Units decomposed for independent testing?
    - Following the file structure from the plan?
    - Did this change create or grow files significantly beyond what the task required?
    - Repo conventions from CLAUDE.md: no unwrap() outside tests; errors written for an LLM caller
      with actionable messages; comments that explain WHY, not what the next line does; the
      host-swap RwLock rule and the ProgressDispatcher forwarder-abort pattern held if touched.

    Report: Strengths, Issues (Critical / Important / Minor), Assessment.
```

---

## Final Code Review — Phase 3 Step H — `agent_type: "code-review"`, `mode: "sync"`

```
task tool:
  agent_type: code-review
  mode: sync
  description: "Final review: <slug>"
  prompt: |
    Final review of the complete implementation.

    Plan (committed at docs/superpowers/plans/<file>, full text inline):
    <PASTE FULL PLAN TEXT>
    Spec (committed at docs/superpowers/specs/<file>, full text inline):
    <PASTE FULL SPEC TEXT>
    Branch: <branch-name>
    Diff range: <merge-base-with-main>..HEAD

    Verify:
    - All plan tasks are implemented end-to-end
    - The implementation actually achieves the spec's success criteria
    - No dead code, leftover debug, or skipped tests
    - Test coverage is reasonable for what was built
    - Repo convention compliance (CLAUDE.md + the SKILL.md conventions table): out-of-band artifacts
      handled (build, plugin examples, provider streaming), commit/branch conventions followed, no
      AI self-attribution anywhere in the diff, CHANGELOG.md updated if this is the release-cut task.

    Report: Strengths, Issues, Overall assessment (Ready to merge / Needs work).
```

---

## Mandatory Review Trio — Phase 4 Step 8

All three are dispatched via the `task` tool, in parallel (same response, or `mode: "background"`
for all three), on every PR, with no size-based carve-out (Non-Negotiable Rules 4–5).

### Rust-expert pass — `agent_type: "general-purpose"`

```
task tool:
  agent_type: general-purpose
  mode: sync   # or background if dispatched alongside the other two
  description: "Rust review: PR #<N>"
  prompt: |
    Review the Rust in PR #<N> of savvagent/savvagent-cli (`gh pr diff <N>`), for ref <ref>.

    You are acting as a senior Rust engineer reviewer. Judge idiomatic Rust against this repo's
    conventions (read crates/*/Cargo.toml and CLAUDE.md first if unfamiliar):
    - Ownership, lifetimes, and borrow discipline; no needless clones or Arc<Mutex<...>> where a
      value would do
    - Error handling: no unwrap()/expect() outside tests, no panics on untrusted input, no silent
      fallback on a resolution failure, errors written for an LLM caller that has never read the docs
    - Async: Send + Sync seams, no blocking work in an async task, the host-swap RwLock rule (never
      hold Arc<RwLock<Option<Arc<Host>>>>'s read guard across an .await) if touched, the rmcp
      ProgressDispatcher forwarder-abort pattern if a streaming path is touched
    - rmcp/MCP usage: tool input schemas well-typed, stdio transport wired through ToolRegistry, no
      HTTP creeping into what should be an in-process or stdio call
    - Tests: this repo has no database and no external-service test dependency — tests should be
      self-contained; a new tool needs coverage of its MCP schema/dispatch, a new provider needs
      coverage via the cross-vendor test matrix pattern if applicable

    Report: Strengths, Issues (Critical / Important / Minor), Assessment.
```

### Architecture pass — `agent_type: "general-purpose"`

```
task tool:
  agent_type: general-purpose
  mode: sync   # or background if dispatched alongside the other two
  description: "Architecture review: PR #<N>"
  prompt: |
    Review PR #<N> of savvagent/savvagent-cli (`gh pr diff <N>`) for architectural consistency, for
    ref <ref>. Read CLAUDE.md at the repo root first — it is the conventions document of record.

    Judge:
    - Everything-is-MCP-shaped: is a provider still just a ProviderHandler, and a tool still just a
      stdio MCP server? Does anything try to special-case a provider or tool inside Host instead of
      going through the ProviderClient/ToolRegistry traits?
    - Crate boundaries: savvagent-protocol holds pure types with no I/O; savvagent-mcp owns the
      ProviderClient/ProviderHandler traits and the InProcessProviderClient bridge; savvagent-host
      owns the turn loop, ToolRegistry, and session state; provider-* / tool-* are implementations;
      crates/savvagent is a thin TUI shell
    - The host-swap rule: Arc<RwLock<Option<Arc<Host>>>>, read lock held only briefly and dropped
      before any .await; /connect swaps the slot atomically
    - The provider transport split: InProcessProviderClient is the default; MCP-over-HTTP is opt-in
      via SAVVAGENT_PROVIDER_URL; Host only ever sees Box<dyn ProviderClient> and gains no registry
    - Whether a public-interface change (SPP wire format, tool schema, plugin ABI, slash command, env
      var, on-disk transcript/keyring format) is additive, and if not, whether it is named in the
      spec, the plan, the CHANGELOG, and the PR body (Non-Negotiable Rule 6)
    - Alignment with the committed spec and plan

    Report: Strengths, Issues (Critical / Important / Minor), Assessment.
```

### Independent security review — `agent_type: "security-review"`

**Blind by construction.** This dispatch receives the diff and nothing else — no spec, no plan, no
task brief, no PR-body summary, no implementer report (Non-Negotiable Rule 5). Keep the "do not
read" instruction attached to the prompt. Per "How dispatch works in this environment" in SKILL.md,
this CLI's `security-review` agent has a mandatory table-plus-`ask_user` output contract; this
dispatch's prompt tells it to skip the `ask_user` follow-up so a fully autonomous run doesn't stall —
you (the orchestrator) apply the fix-loop rule instead once its findings table comes back.

```
task tool:
  agent_type: security-review
  mode: sync   # or background if dispatched alongside the other two
  description: "Security review: PR #<N>"
  prompt: |
    Perform an independent security review of PR #<N> in savvagent/savvagent-cli.

    Read ONLY the diff: `gh pr diff <N>`.
    Do NOT read the PR description, the issue, the spec, the plan, or any summary of intent. Your
    findings must come from the code as written, so they cannot be steered by the author's framing.

    This is a terminal coding agent with real execution power. Weigh these hardest:
    - Secrets: does anything write an API key, token, or other secret to disk in plaintext (a
      transcript JSON, a log line, an error message, a response) instead of the OS keyring (service
      `savvagent`)? Is a secret ever echoed back to the model or the terminal?
    - Sandboxing / execution surface: does tool-bash gain unbounded shell execution beyond its
      existing contract? Does tool-web gain unbounded network egress or SSRF exposure? Does a plugin
      (savvagent-plugin-wasm / savvagent-plugin-wit) gain filesystem or process-spawning reach beyond
      its declared capability surface?
    - Injection: path traversal in tool-fs/tool-grep, command injection in tool-bash, unsafe
      deserialization of provider/tool responses
    - The plugin ABI: is untrusted plugin code given more trust than the WASM/WIT sandbox boundary
      intends?
    - Denial of service: unbounded reads, infinite loops in the turn loop, a resource never released
      (a spawned child process, an open file handle)
    - Anything that fails open where it should fail closed

    After your investigation, present findings as the standard severity table (🔴 CRITICAL / 🟠 HIGH
    / 🟡 MEDIUM / ⚪ LOW) with file:line refs and a concrete fix each, then an overall assessment.

    Do NOT invoke ask_user or otherwise wait for human follow-up input — this review runs inside a
    fully autonomous pipeline with no human present to answer. End your report with the findings
    table and overall assessment; the orchestrating session will decide on fixes.
```

---

## Review-Response Subagent — Phase 4 Step 9 — `agent_type: "general-purpose"`

```
task tool:
  agent_type: general-purpose
  mode: sync   # or background if you want to continue other work while it runs
  description: "Address PR review feedback"
  prompt: |
    You are addressing PR review feedback on PR #<N> of savvagent/savvagent-cli, for <ref>.
    Follow savvagent-development requirements (Phase 4 step 9).

    Your job:
    - Read all unresolved review threads on the PR
    - For each comment: either fix-and-reply ("Fixed in <sha>") or explicitly dismiss with
      reasoning. NEVER silent dismissal.
    - Work in the existing worktree; run `cargo fmt --all` before every Rust commit, and
      `cargo test --workspace --no-fail-fast` + `cargo clippy --workspace --all-targets` before
      pushing.
    - After each reply, resolve the conversation thread via GraphQL:
        gh api graphql -f query='mutation {
          resolveReviewThread(input: {threadId: "<thread_id>"}) {
            thread { isResolved }
          }
        }'
    - Always reply inline to each comment explaining how the feedback was addressed
      (keeps the review thread traceable).
    - For automated reviewers, a flagged false positive should be verified (e.g. with
      `cat -A` for whitespace/table-formatting flags) then dismissed with reasoning.
    - Never add AI self-attribution to a commit, a reply, or the PR body.
    - If the same thread remains unresolved across multiple dispatch runs, escalate
      (do not silently retry).

    Return when all threads are resolved or escalation is needed. The orchestrating session
    receives only the summary (what was fixed, what was dismissed, any escalations).
```
