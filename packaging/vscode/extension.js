// Spid3r language client.
//
// Deliberately dependency-free: it spawns `spid3r lsp` as a child process and
// forwards the JSON-RPC stream. No `vscode-languageclient` package, so the
// extension installs and runs with `npm install` doing nothing at all.

const vscode = require("vscode");
const cp = require("child_process");
const path = require("path");

/** @type {vscode.OutputChannel} */
let output;

function log(line) {
  if (output) {
    output.appendLine(line);
  }
}

function activate(context) {
  output = vscode.window.createOutputChannel("Spid3r");
  log("[spid3r] activating");

  const cfg = vscode.workspace.getConfiguration("spid3r");
  const serverPath = cfg.get("serverPath") || "spid3r";
  const trace = cfg.get("trace.server") || "off";

  const serverOptions = {
    command: serverPath,
    args: ["lsp"],
    transport: undefined, // stdio by default
    options: { stdio: ["pipe", "pipe", "pipe"] },
  };

  // The client is hand-rolled: start the child, and let VS Code's built-in
  // LSP transport handle framing once we hand it the streams. `vscode-languageclient`
  // would do this for us, but it is a large dependency for ~200 lines of glue.
  let child;
  try {
    child = cp.spawn(serverPath, ["lsp"], {
      stdio: ["pipe", "pipe", "pipe"],
      // A GUI app must not open a console window on Windows.
      windowsHide: true,
    });
  } catch (e) {
    vscode.window.showErrorMessage(
      `Spid3r: could not start '${serverPath} lsp' — set "spid3r.serverPath" to the spid3r executable.`
    );
    log(`[spid3r] spawn failed: ${e}`);
    return;
  }

  child.on("error", (e) => {
    vscode.window.showErrorMessage(
      `Spid3r: failed to run '${serverPath} lsp'. Set "spid3r.serverPath" to the spid3r executable.`
    );
    log(`[spid3r] child error: ${e}`);
  });
  child.on("exit", (code) => log(`[spid3r] server exited with ${code}`));
  child.stderr.on("data", (d) => log(`[spid3r] ${String(d).trimEnd()}`));

  if (trace !== "off") {
    child.stdout.on("data", (d) => {
      if (trace === "verbose") log(`<-- ${String(d).trimEnd()}`);
    });
  }

  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "spid3r" }],
    outputChannel: output,
    // The server pushes diagnostics itself on open/change.
    synchronize: { fileEvents: [] },
    revealOutputChannelOn: 4, // never pop the channel on an error
  };

  // `vscode-languageclient` is not bundled, so drive the protocol directly:
  // VS Code exposes the LSP endpoint over the child's stdio and this minimal
  // client shims the `initialize` handshake itself. To keep this file free of
  // dependencies we register the language features against the raw endpoint.
  const client = createMinimalClient(child, clientOptions);
  context.subscriptions.push(client);
  log("[spid3r] client started");
}

/**
 * A dependency-free LSP client.
 *
 * VS Code does not export a way to attach an arbitrary JSON-RPC endpoint to
 * its language-feature registry, so this implementation speaks the protocol
 * itself: it frames messages to the server, and registers the document
 * listeners that feed it.
 */
function createMinimalClient(child, options) {
  let nextId = 1;
  let initialized = false;

  function send(method, params, id) {
    const msg = { jsonrpc: "2.0", method, params };
    if (id !== undefined) {
      msg.id = id;
    }
    const body = Buffer.from(JSON.stringify(msg), "utf8");
    child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    child.stdin.write(body);
  }

  function sendRequest(method, params) {
    const id = nextId++;
    send(method, params, id);
    return id;
  }

  // ---- handshake
  sendRequest("initialize", {
    processId: process.pid,
    rootUri: vscode.workspace.workspaceFolders
      ? vscode.workspace.workspaceFolders[0].uri.toString()
      : null,
    capabilities: {},
    clientInfo: { name: "vscode-spid3r", version: "1.0.0" },
  });
  send("initialized", {});
  initialized = true;

  // ---- document sync
  for (const evt of [vscode.workspace.onDidOpenTextDocument, vscode.workspace.onDidSaveTextDocument]) {
    vscode.workspace.onDidOpenTextDocument((doc) => {
      if (doc.languageId !== "spid3r") return;
      send("textDocument/didOpen", {
        textDocument: {
          uri: doc.uri.toString(),
          languageId: doc.languageId,
          version: doc.version,
          text: doc.getText(),
        },
      });
    });
  }

  vscode.workspace.onDidChangeTextDocument((e) => {
    if (e.document.languageId !== "spid3r") return;
    send("textDocument/didChange", {
      textDocument: { uri: e.document.uri.toString(), version: e.document.version },
      contentChanges: [{ text: e.document.getText() }],
    });
  });

  vscode.workspace.onDidCloseTextDocument((doc) => {
    if (doc.languageId !== "spid3r") return;
    send("textDocument/didClose", {
      textDocument: { uri: doc.uri.toString() },
    });
  });

  // ---- diagnostics
  const diagnostics = vscode.languages.createDiagnosticCollection("spid3r");
  let buffer = "";
  child.stdout.on("data", (chunk) => {
    buffer += String(chunk);
    while (true) {
      const headerEnd = buffer.indexOf("\r\n\r\n");
      if (headerEnd < 0) return;
      const header = buffer.slice(0, headerEnd);
      const m = /Content-Length: (\d+)/i.exec(header);
      if (!m) {
        buffer = buffer.slice(headerEnd + 4);
        continue;
      }
      const len = parseInt(m[1], 10);
      if (buffer.length < headerEnd + 4 + len) return; // wait for the body
      const body = buffer.slice(headerEnd + 4, headerEnd + 4 + len);
      buffer = buffer.slice(headerEnd + 4 + len);
      handle(JSON.parse(body));
    }
  });

  function handle(msg) {
    if (msg.method === "textDocument/publishDiagnostics") {
      const uri = vscode.Uri.parse(msg.params.uri);
      const items = (msg.params.diagnostics || []).map(toDiagnostic);
      diagnostics.set(uri, items);
      return;
    }
    if (msg.method && msg.method.startsWith("$/")) return;
    log(`--> ${JSON.stringify(msg)}`);
  }

  function toDiagnostic(d) {
    const range = new vscode.Range(
      d.range.start.line,
      d.range.start.character,
      d.range.end.line,
      d.range.end.character
    );
    const severity =
      d.severity === 1
        ? vscode.DiagnosticSeverity.Error
        : d.severity === 2
        ? vscode.DiagnosticSeverity.Warning
        : vscode.DiagnosticSeverity.Information;
    const item = new vscode.Diagnostic(range, d.message, severity);
    item.source = d.source || "spid3r";
    return item;
  }

  return {
    dispose() {
      diagnostics.dispose();
      send("shutdown", null);
      send("exit", null);
      child.kill();
    },
    initialized,
    send,
  };
}

function deactivate() {
  // Nothing to do: the server exits when stdin closes.
}

module.exports = { activate, deactivate };
