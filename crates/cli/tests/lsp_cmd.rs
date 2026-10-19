//! End-to-end test for `meshfox lsp`: the real binary on stdio, speaking
//! the protocol — initialize, an open buffer with an error, the outline,
//! the "turn this heading into a node" code action, and a clean shutdown.
//! The buffer is never a file on disk: the server must work on the text the
//! editor sends.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Client {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_meshfox"))
            .arg("lsp")
            // Nothing here may depend on (or be routed through) the
            // developer's own config.
            .env("HOME", std::env::temp_dir().join("meshfox-lsp-test-home"))
            .env("MESHFOX_SERVER_SOCKET", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn meshfox lsp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Client {
            child,
            stdin,
            stdout,
            next_id: 0,
        }
    }

    fn send(&mut self, msg: Value) {
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut len = 0;
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed"
            );
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                len = v.parse().unwrap();
            }
        }
        let mut buf = vec![0; len];
        self.stdout.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let msg = self.read();
            if msg.get("id") == Some(&json!(id)) {
                return msg;
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// The next `publishDiagnostics` for `uri`.
    fn diagnostics(&mut self, uri: &str) -> Vec<Value> {
        loop {
            let msg = self.read();
            if msg["method"] == "textDocument/publishDiagnostics" && msg["params"]["uri"] == uri {
                return msg["params"]["diagnostics"].as_array().unwrap().clone();
            }
        }
    }
}

const URI: &str = "file:///nonexistent/dir/doc.canvas.md";
const DOC: &str = "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n## Tests\n<!-- meshfox:node id=\"tests\" -->\n\n## Plain\ntext\n";

fn open(client: &mut Client, text: &str) {
    client.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": URI, "languageId": "markdown", "version": 1, "text": text}}),
    );
}

#[test]
fn serves_diagnostics_outline_and_code_actions_for_an_unsaved_buffer() {
    let mut c = Client::start();
    let init = c.request(
        "initialize",
        json!({"processId": null, "rootUri": null, "capabilities": {}}),
    );
    let caps = &init["result"]["capabilities"];
    assert_eq!(caps["documentSymbolProvider"], true);
    assert!(
        caps["completionProvider"]["triggerCharacters"].is_array(),
        "{caps}"
    );
    assert!(caps["codeActionProvider"].is_object(), "{caps}");
    c.notify("initialized", json!({}));

    // A valid buffer: no diagnostics, and the outline nests.
    open(&mut c, DOC);
    assert!(c.diagnostics(URI).is_empty());
    let symbols = c.request(
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": URI}}),
    );
    let root = &symbols["result"][0];
    assert_eq!(root["name"], "Root");
    assert_eq!(root["children"][0]["name"], "Tests");

    // The Ctrl-n equivalent, offered on the unmarked heading.
    let actions = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": URI},
               "range": {"start": {"line": 7, "character": 0}, "end": {"line": 7, "character": 0}},
               "context": {"diagnostics": []}}),
    );
    let action = &actions["result"][0];
    assert!(
        action["title"].as_str().unwrap().contains("Plain"),
        "{action}"
    );
    let edit = &action["edit"]["changes"][URI][0];
    assert_eq!(edit["range"]["start"], json!({"line": 8, "character": 0}));
    assert!(edit["newText"]
        .as_str()
        .unwrap()
        .starts_with("<!-- meshfox:node id=\""));

    // Completion: the snippet for a marker being typed, and the values of
    // an attribute, from the document's own nodes.
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": URI, "version": 3},
               "contentChanges": [{"text": format!("{DOC}\n<!-- m")}]}),
    );
    let _ = c.diagnostics(URI);
    let items = c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": URI}, "position": {"line": 10, "character": 6}}),
    );
    let node = items["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "meshfox:node")
        .unwrap_or_else(|| panic!("{items}"))
        .clone();
    assert_eq!(node["insertTextFormat"], 2);
    assert_eq!(node["textEdit"]["range"]["end"]["character"], 6);

    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": URI, "version": 4},
               "contentChanges": [{"text": DOC.replace("id=\"tests\"", "id=\"tests\" parent=\"\"")}]}),
    );
    let _ = c.diagnostics(URI);
    let col = "<!-- meshfox:node id=\"tests\" parent=\"".len();
    let items = c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": URI}, "position": {"line": 5, "character": col}}),
    );
    let labels: Vec<_> = items["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(labels, ["root"], "{items}");

    // Break it: a duplicate id is reported on the second marker.
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": URI, "version": 2},
               "contentChanges": [{"text": DOC.replace("id=\"tests\"", "id=\"root\"")}]}),
    );
    let diags = c.diagnostics(URI);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0]["severity"], 1);
    assert_eq!(diags[0]["range"]["start"]["line"], 5);
    assert_eq!(diags[0]["source"], "meshfox");

    // Closing clears them.
    c.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": URI}}),
    );
    assert!(c.diagnostics(URI).is_empty());

    let down = c.request("shutdown", Value::Null);
    assert!(down["error"].is_null(), "{down}");
    c.notify("exit", Value::Null);
    let status = c.child.wait().unwrap();
    assert!(status.success(), "{status:?}");
}
