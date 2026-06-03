#!/usr/bin/env node
// Integration smoke test: drive the installed `shell-acp` over ACP (JSON-RPC
// 2.0 over stdio) and confirm a real command's output comes back.
//
//   node samples/acp-smoke.js            # spawns `shell-acp` from PATH
//   SHELL_ACP_BIN=/path/to/shell-acp node samples/acp-smoke.js

const { spawn } = require("child_process");

const BIN = process.env.SHELL_ACP_BIN || "shell-acp";
const MARK = "shell-acp-smoke-ok";

const proc = spawn(BIN, [], { stdio: ["pipe", "pipe", "inherit"] });

let buf = "";
let done = false;
const send = (o) => proc.stdin.write(JSON.stringify(o) + "\n");
const fail = (m) => {
  console.error("SMOKE FAIL:", m);
  try { proc.kill(); } catch {}
  process.exit(1);
};

const timer = setTimeout(() => fail("timed out waiting for command output"), 25000);

proc.on("error", (e) => fail("spawn error: " + e.message));
proc.on("exit", (code) => { if (!done) fail("process exited early, code=" + code); });

proc.stdout.on("data", (d) => {
  buf += d.toString();
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i);
    buf = buf.slice(i + 1);
    if (!line.trim()) continue;
    let msg;
    try { msg = JSON.parse(line); } catch { continue; }

    // session/new result → fire the command
    if (msg.id === 2 && msg.result && msg.result.sessionId) {
      send({
        jsonrpc: "2.0", id: 3, method: "session/prompt",
        params: { sessionId: msg.result.sessionId, prompt: [{ type: "text", text: `echo ${MARK}` }] },
      });
    }
    // streamed output → look for the marker
    if (msg.method === "session/update") {
      const text = msg.params?.update?.content?.text || "";
      if (text.includes(MARK)) {
        done = true;
        clearTimeout(timer);
        console.log("SMOKE OK: shell-acp executed a command and streamed output back");
        try { proc.kill(); } catch {}
        process.exit(0);
      }
    }
  }
});

send({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} });
setTimeout(
  () => send({ jsonrpc: "2.0", id: 2, method: "session/new", params: { cwd: process.cwd() } }),
  300
);
