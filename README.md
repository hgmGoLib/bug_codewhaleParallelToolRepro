# Codewhale (anthropic provider): parallel tool calls get a fake "tool call was not executed" result

Deterministic repro in one `cargo run` - **no API key, no real model, nothing to set up
besides a Rust toolchain and a codewhale binary**. A small Rust program (only dependency:
`serde_json`) plays a scripted Anthropic Messages API on localhost that always answers
with **two parallel `tool_use` blocks** in one message, runs codewhale once against it,
and checks the request codewhale sends back.

- Codewhale version: **0.9.13** (`a0b81f619b66`)
- Verified on Windows 11 (codewhale from `npm install -g codewhale`). Nothing in the repro
  or in the bug is platform specific.
- Provider: `anthropic`

## Summary

When the model returns **one assistant message with 2+ `tool_use` blocks**, the next
request Codewhale sends contains, for the 2nd (and later) tool call, **two** results:

1. a placeholder `{"is_error": true, "content": "tool call was not executed"}`, and
2. the real result of the tool (the tool *was* executed successfully).

The model is told the same call both failed and succeeded - a contradictory history on
every turn that contains parallel tool calls.

## Steps to reproduce (Windows, `cmd.exe`)

Verified exactly as written below on Windows 11 in a plain `cmd.exe` window. Only
Node.js/npm and Git are assumed to be installed. (Use `cmd.exe`, not PowerShell: with
the default PowerShell execution policy `npm` refuses to run.)

```bat
:: 1. codewhale 0.9.13
npm install -g codewhale@0.9.13

:: 2. Rust toolchain - skip this step if `cargo --version` already works.
::    The GNU host toolchain needs no Visual Studio / Build Tools.
curl -L -o rustup-init.exe https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-gnu/rustup-init.exe
rustup-init.exe -y --profile minimal --default-host x86_64-pc-windows-gnu
set PATH=%USERPROFILE%\.cargo\bin;%PATH%

:: 3. build and run the repro
git clone https://github.com/hgmGoLib/bug_codewhaleParallelToolRepro.git
cd bug_codewhaleParallelToolRepro
cargo run
```

`cargo run` finds the real binary of the npm install at
`%APPDATA%\npm\node_modules\codewhale\bin\downloads\codewhale.exe` by itself. To test
another build (e.g. one built from source) pass it explicitly:
`cargo run -- path\to\codewhale.exe` (or set `CODEWHALE_BIN`). On Linux/macOS the same
`cargo run -- /path/to/codewhale` works; only Windows was verified.

Exit code: `1` = bug reproduced, `0` = history is correct, `2` = setup problem.
So when the bug is present, `cargo run` ends with
`error: process didn't exit successfully: ... (exit code: 1)` - that is the expected result.

Notes from the verified run (Windows 11, npm 11.19.0, Git for Windows, rustc 1.98.1
`stable-x86_64-pc-windows-gnu` installed by step 2):

- npm 11 prints `npm warn install-scripts ... codewhale@0.9.13 (postinstall ...)`; the
  codewhale binary was still downloaded to the path above, the warning can be ignored.
- `rustup-init` says "you may need to restart your current shell"; the `set PATH=...` line
  in step 2 makes that unnecessary.

What `src/main.rs` does:

1. starts the fake Anthropic Messages API on `127.0.0.1:<random port>`:

   | request | fake model reply |
   |---|---|
   | last message has no `tool_result` | text + `tool_use toolu_repro_1_a read{path:"a.txt"}` + `tool_use toolu_repro_1_b read{path:"b.txt"}`, `stop_reason=tool_use` |
   | last message has `tool_result` | text "I read both files. Done.", `stop_reason=end_turn` |

2. creates a temp dir with an isolated `CODEWHALE_HOME` and a work dir containing `a.txt`, `b.txt`.
   The config (the key is fake, the server does not check it):
   ```toml
   provider = "anthropic"
   approval_policy = "never"
   sandbox_mode = "danger-full-access"
   telemetry = false

   [update]
   check_for_updates = false

   [providers.anthropic]
   api_key = "sk-ant-fake-key-not-checked"
   base_url = "http://127.0.0.1:<port>"
   model = "claude-sonnet-4-6"
   ```
3. runs once, in the work dir:
   ```
   codewhale --config <home>/config.toml exec --auto --output-format stream-json "Read a.txt and b.txt and tell me what they say."
   ```
4. saves every request body as `req_<n>.json` (plus `codewhale.log`) in the temp dir and
   prints, for every `tool_use` in the history, all `tool_result` blocks that answer it.

## Actual result (real run, codewhale 0.9.13)

Output of `cargo run` from step 3 (cargo's own build lines omitted):

```
codewhale binary: C:\Users\<user>\AppData\Roaming\npm\node_modules\codewhale\bin\downloads\codewhale.exe
codewhale 0.9.13 (a0b81f619b66)
fake Anthropic Messages API: http://127.0.0.1:55526
temp dir: C:\Users\<user>\AppData\Local\Temp\codewhale-parallel-tool-repro-3784
POST /v1/messages req_1: stream=true messages=1 tools=11
POST /v1/messages req_2: stream=true messages=4 tools=11
codewhale exited: exit code: 0

requests received: 2
[req_2] message roles: ["user", "assistant", "user", "user"]
[req_2]   tool_use toolu_repro_1_a (read) in messages[1] -> 1 tool_result(s)
[req_2]     messages[2] is_error=false content="content of file A"
[req_2]   tool_use toolu_repro_1_b (read) in messages[1] -> 2 tool_result(s)
[req_2]     messages[2] is_error=true content="tool call was not executed"
[req_2]     messages[3] is_error=false content="content of file B"

>>> BUG REPRODUCED: a tool call that was executed also got a 'tool call was not executed' placeholder result (contradictory history sent to the model)
request bodies + codewhale.log: C:\Users\<user>\AppData\Local\Temp\codewhale-parallel-tool-repro-3784
error: process didn't exit successfully: `target\debug\codewhale-parallel-tool-repro.exe` (exit code: 1)
```

`messages` of req_2 exactly as Codewhale sent them (full file: `sample/req_2.messages.json`;
the `<turn_meta>` text block of the first user message is shortened here):

```json
[
  {"role": "user", "content": [
    {"type": "text", "text": "Read a.txt and b.txt and tell me what they say."},
    {"type": "text", "text": "<turn_meta>...</turn_meta>"}]},
  {"role": "assistant", "content": [
    {"type": "text", "text": "I will read both files in parallel."},
    {"type": "tool_use", "id": "toolu_repro_1_a", "name": "read", "input": {"path": "a.txt"}},
    {"type": "tool_use", "id": "toolu_repro_1_b", "name": "read", "input": {"path": "b.txt"}}]},
  {"role": "user", "content": [
    {"type": "tool_result", "tool_use_id": "toolu_repro_1_b", "content": "tool call was not executed", "is_error": true},
    {"type": "tool_result", "tool_use_id": "toolu_repro_1_a", "content": "content of file A"}]},
  {"role": "user", "content": [
    {"type": "tool_result", "tool_use_id": "toolu_repro_1_b", "content": "content of file B",
     "cache_control": {"type": "ephemeral"}}]}
]
```

## Expected result

One `user` message right after the assistant message, with exactly one `tool_result`
per `tool_use` and no placeholder:

```json
{"role": "user", "content": [
  {"type": "tool_result", "tool_use_id": "toolu_repro_1_a", "content": "content of file A"},
  {"type": "tool_result", "tool_use_id": "toolu_repro_1_b", "content": "content of file B"}]}
```

## Where it comes from (reading the source on `main`)

1. The engine stores **each** tool result as its own user message, so a 2-call turn becomes
   `assistant{use_a, use_b}`, `user{result_a}`, `user{result_b}`.
2. The OpenAI Chat Completions path merges adjacent user messages
   (`crates/tui/src/client/chat.rs`, `merge_adjacent_user_content`), which is why the same
   scenario is fine on OpenAI-wire providers. The Anthropic path
   (`crates/tui/src/client/anthropic.rs`) does not merge them.
3. `repair_dangling_tool_uses` (added for #5002 in #5063) then looks for each `tool_use`'s
   result **only in the immediately following user message**. `result_b` sits in the
   message after that, so `use_b` is treated as dangling and gets the
   `"tool call was not executed"` placeholder prepended - while the real `result_b` stays
   in the next message.

Before #5063 the same history made the API return the 400
"`tool_use` ids were found without `tool_result` blocks immediately after" (#4329, #5002),
which is consistent with this root cause. Merging adjacent user messages on the Anthropic
path before the repair step (like the chat path does) would fix both.

## Files

| file | purpose |
|---|---|
| `Cargo.toml`, `Cargo.lock`, `src/main.rs` | the repro: fake Anthropic server + codewhale driver + checker (edition 2024, rust-version 1.88, same as codewhale) |
| `sample/req_2.messages.json` | `messages` of the 2nd request from a real run (user name in the workspace path replaced by `<user>`) |
