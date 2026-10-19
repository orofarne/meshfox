//! `meshfox lsp` — a Language Server Protocol server on stdio, so editors
//! other than the TUI get what its source editor has: diagnostics, an
//! outline, and (next) completion and code actions.
//!
//! It analyses the text the editor sends (`analysis.rs`, pure functions of
//! that text) and never opens a canvas file itself. Anything about the
//! canvas's *saved* state — other files, grants, constraints, run history —
//! belongs to the file's worker, reached through `worker_client` like the
//! TUI and `meshfox mcp` do. Edits go back to the editor as edits of its
//! own buffer; the editor saves, the worker sees an ordinary external
//! change.

mod analysis;

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{CodeActionRequest, Completion, DocumentSymbolRequest, Request as _};
use lsp_types::{
    CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CompletionOptions, CompletionParams, CompletionResponse,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSymbolParams, DocumentSymbolResponse, OneOf, PublishDiagnosticsParams,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};

pub fn run() -> Result<(), String> {
    let (connection, io_threads) = Connection::stdio();
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_symbol_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some([" ", "\"", ",", "/", "<", "$"].map(String::from).to_vec()),
            ..Default::default()
        }),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::REFACTOR_REWRITE]),
            ..Default::default()
        })),
        ..Default::default()
    };
    connection
        .initialize(serde_json::to_value(capabilities).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let result = Server::default().main_loop(&connection);
    // The writer thread only ends once every sender is gone.
    drop(connection);
    io_threads.join().map_err(|e| e.to_string())?;
    result
}

/// The open buffers, by URI.
#[derive(Default)]
struct Server {
    docs: HashMap<Uri, String>,
}

impl Server {
    fn main_loop(&mut self, connection: &Connection) -> Result<(), String> {
        for message in &connection.receiver {
            match message {
                Message::Request(req) => {
                    if connection
                        .handle_shutdown(&req)
                        .map_err(|e| e.to_string())?
                    {
                        return Ok(());
                    }
                    let response = self.request(req);
                    send(connection, Message::Response(response))?;
                }
                Message::Notification(note) => {
                    for out in self.notification(note) {
                        send(connection, Message::Notification(out))?;
                    }
                }
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    fn request(&self, req: Request) -> Response {
        let id = req.id.clone();
        match req.method.as_str() {
            DocumentSymbolRequest::METHOD => {
                match req.extract::<DocumentSymbolParams>(DocumentSymbolRequest::METHOD) {
                    Ok((id, params)) => {
                        let symbols = self
                            .docs
                            .get(&params.text_document.uri)
                            .map(|text| analysis::symbols(text))
                            .unwrap_or_default();
                        ok(id, DocumentSymbolResponse::Nested(symbols))
                    }
                    Err(e) => invalid(id, e),
                }
            }
            Completion::METHOD => match req.extract::<CompletionParams>(Completion::METHOD) {
                Ok((id, params)) => {
                    let at = params.text_document_position;
                    let items = self
                        .docs
                        .get(&at.text_document.uri)
                        .map(|text| analysis::completion(text, at.position))
                        .unwrap_or_default();
                    ok(id, CompletionResponse::Array(items))
                }
                Err(e) => invalid(id, e),
            },
            CodeActionRequest::METHOD => {
                match req.extract::<CodeActionParams>(CodeActionRequest::METHOD) {
                    Ok((id, params)) => {
                        let uri = params.text_document.uri;
                        let actions: Vec<CodeActionOrCommand> = self
                            .docs
                            .get(&uri)
                            .map(|text| analysis::code_actions(text, &uri, params.range))
                            .unwrap_or_default()
                            .into_iter()
                            .map(CodeActionOrCommand::CodeAction)
                            .collect();
                        ok(id, actions)
                    }
                    Err(e) => invalid(id, e),
                }
            }
            other => Response::new_err(
                id,
                ErrorCode::MethodNotFound as i32,
                format!("unsupported request {other}"),
            ),
        }
    }

    /// Handles a notification; returns the notifications to send back.
    fn notification(&mut self, note: Notification) -> Vec<Notification> {
        match note.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let Ok(p) = note.extract::<DidOpenTextDocumentParams>(DidOpenTextDocument::METHOD)
                else {
                    return Vec::new();
                };
                let (uri, version) = (p.text_document.uri, p.text_document.version);
                self.docs.insert(uri.clone(), p.text_document.text);
                vec![self.publish(&uri, Some(version))]
            }
            DidChangeTextDocument::METHOD => {
                let Ok(p) =
                    note.extract::<DidChangeTextDocumentParams>(DidChangeTextDocument::METHOD)
                else {
                    return Vec::new();
                };
                // Full sync: the last change is the whole new text.
                let Some(change) = p.content_changes.into_iter().last() else {
                    return Vec::new();
                };
                let uri = p.text_document.uri;
                self.docs.insert(uri.clone(), change.text);
                vec![self.publish(&uri, Some(p.text_document.version))]
            }
            DidCloseTextDocument::METHOD => {
                let Ok(p) =
                    note.extract::<DidCloseTextDocumentParams>(DidCloseTextDocument::METHOD)
                else {
                    return Vec::new();
                };
                self.docs.remove(&p.text_document.uri);
                // Leave nothing behind in the editor's diagnostics list.
                vec![publication(p.text_document.uri, Vec::new(), None)]
            }
            _ => Vec::new(),
        }
    }

    fn publish(&self, uri: &Uri, version: Option<i32>) -> Notification {
        let diagnostics = self
            .docs
            .get(uri)
            .map(|text| analysis::diagnostics(text))
            .unwrap_or_default();
        publication(uri.clone(), diagnostics, version)
    }
}

fn publication(
    uri: Uri,
    diagnostics: Vec<lsp_types::Diagnostic>,
    version: Option<i32>,
) -> Notification {
    Notification::new(
        PublishDiagnostics::METHOD.to_string(),
        PublishDiagnosticsParams {
            uri,
            diagnostics,
            version,
        },
    )
}

fn ok<T: serde::Serialize>(id: lsp_server::RequestId, result: T) -> Response {
    Response::new_ok(id, result)
}

fn invalid(id: lsp_server::RequestId, e: lsp_server::ExtractError<Request>) -> Response {
    Response::new_err(id, ErrorCode::InvalidParams as i32, e.to_string())
}

fn send(connection: &Connection, message: Message) -> Result<(), String> {
    connection.sender.send(message).map_err(|e| e.to_string())
}
