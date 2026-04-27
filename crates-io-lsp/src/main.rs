use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use cargo_manifest::Dependency;
use clap::Parser;
use tokio::sync::RwLock;
use toml::Spanned;
use tower_lsp_server::ls_types::{
    self, DiagnosticServerCapabilities, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentDiagnosticParams, DocumentDiagnosticReportResult, InitializeParams, InitializeResult,
    MessageType, Position, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
    Uri,
};
use tower_lsp_server::{Client, LanguageServer, LspService, Server, jsonrpc};

mod api;

#[derive(Parser, Debug, Clone)]
struct Args {
    #[arg(short, long, default_value = "https://index.crates.io")]
    endpoint: String,
    #[arg(short, long, default_value = "")]
    token: String,
}

struct CratesIoBackend {
    client: Client,
    endpoint: String,
    token: String,
    open_docs: RwLock<HashMap<Uri, FileInfo>>,
    cache: RwLock<HashMap<String, Vec<String>>>,
}

impl LanguageServer for CratesIoBackend {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        self.client
            .log_message(
                MessageType::INFO,
                format!("Init {:?}", params.initialization_options),
            )
            .await;
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(ls_types::PositionEncodingKind::UTF8),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                diagnostic_provider: Some(
                    DiagnosticServerCapabilities::Options(Default::default()),
                ),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "DidOpen: {} {}",
                    params.text_document.version,
                    params.text_document.uri.as_str()
                ),
            )
            .await;
        if !is_cargo_toml(&params.text_document.uri) {
            return;
        }

        self.open_docs.write().await.insert(
            params.text_document.uri.clone(),
            FileInfo::new(
                params.text_document.text.clone(),
                params.text_document.version,
            ),
        );
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        self.client
            .log_message(
                MessageType::INFO,
                format!(
                    "DidChange: {} {}",
                    params.text_document.version,
                    params.text_document.uri.as_str()
                ),
            )
            .await;
        if !is_cargo_toml(&params.text_document.uri) {
            return;
        }

        let mut open_docs = self.open_docs.write().await;
        if let Some(doc) = open_docs.get_mut(&params.text_document.uri) {
            for change in params.content_changes {
                if let Some(range) = change.range {
                    if let (Some(start), Some(end)) = (
                        pos_to_offset(&doc.text, range.start),
                        pos_to_offset(&doc.text, range.end),
                    ) {
                        if start <= end && end <= doc.text.len() {
                            doc.text.replace_range(start..end, &change.text);
                        }
                    }
                } else {
                    doc.text = change.text;
                }
            }
            doc.version = params.text_document.version;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.client
            .log_message(
                MessageType::INFO,
                format!("DidSave: {}", params.text_document.uri.as_str()),
            )
            .await;
        if !is_cargo_toml(&params.text_document.uri) {
            return;
        }

        let (text, version) = {
            let mut open_docs = self.open_docs.write().await;
            if let Some(doc) = open_docs.get_mut(&params.text_document.uri) {
                if let Some(text) = &params.text {
                    doc.text = text.clone();
                }
                (doc.text.clone(), Some(doc.version))
            } else if let Some(text) = &params.text {
                (text.clone(), None)
            } else {
                return;
            }
        };
        self.update_diagnostics(&params.text_document.uri, version, &text)
            .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.client
            .log_message(
                MessageType::INFO,
                format!("DidClose: {}", params.text_document.uri.as_str()),
            )
            .await;
        if !is_cargo_toml(&params.text_document.uri) {
            return;
        }

        // Clear diagnostics for the closed document
        self.client
            .publish_diagnostics(params.text_document.uri.clone(), Vec::new(), None)
            .await;

        let mut open_docs = self.open_docs.write().await;
        open_docs.remove(&params.text_document.uri);
    }

    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> jsonrpc::Result<DocumentDiagnosticReportResult> {
        let uri = params.text_document.uri;
        self.client
            .log_message(
                MessageType::INFO,
                format!("Diagnostic request: {}", uri.as_str()),
            )
            .await;
        let doc = self.open_docs.read().await.get(&uri).cloned();
        let Some(text) = doc.map(|d| d.text) else {
            return Err(jsonrpc::Error {
                code: jsonrpc::ErrorCode::InvalidParams,
                message: "Document not found".into(),
                data: None,
            });
        };
        match self.collect_diagnostics(&text).await {
            Ok(diagnostics) => Ok(DocumentDiagnosticReportResult::Report(
                ls_types::DocumentDiagnosticReport::Full(
                    ls_types::RelatedFullDocumentDiagnosticReport {
                        related_documents: None,
                        full_document_diagnostic_report: ls_types::FullDocumentDiagnosticReport {
                            result_id: None,
                            items: diagnostics,
                        },
                    },
                ),
            )),
            Err(err) => {
                self.client
                    .log_message(MessageType::ERROR, format!("Failed diagnostics: {err}"))
                    .await;
                Err(jsonrpc::Error {
                    code: jsonrpc::ErrorCode::InternalError,
                    message: "Failed to collect diagnostics".into(),
                    data: None,
                })
            }
        }
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        self.client.log_message(MessageType::INFO, "Shutdown").await;
        Ok(())
    }
}

impl CratesIoBackend {
    async fn update_diagnostics(&self, uri: &Uri, version: Option<i32>, text: &str) {
        match self.collect_diagnostics(text).await {
            Ok(diagnostics) => {
                self.client
                    .publish_diagnostics(uri.clone(), diagnostics, version)
                    .await
            }
            Err(err) => {
                self.client
                    .log_message(MessageType::ERROR, format!("Failed diagnostics: {err}"))
                    .await
            }
        }
    }

    async fn collect_diagnostics(&self, text: &str) -> Result<Vec<ls_types::Diagnostic>> {
        let parsed: SpannedManifest = toml::from_str(text)?;
        let deps = parsed
            .dependencies
            .iter()
            .chain(parsed.build_dependencies.iter())
            .chain(parsed.dev_dependencies.iter())
            // Filter out relative dependencies
            .filter(|d| !d.1.detail().is_some_and(|d| d.path.is_some()))
            .collect::<Vec<_>>();

        let dep_names = deps
            .iter()
            .map(|(name, _)| name.as_ref().to_string())
            .collect::<Vec<_>>();
        // Fetch versions for dependencies (in parallel)
        let dep_versions = self.get_versions(dep_names).await;

        let mut diagnostics = Vec::new();
        for (name, mut versions) in dep_versions {
            versions.reverse();

            let (name, info) = deps.iter().find(|(n, _)| n.as_ref() == &name).unwrap();

            let range = if let (Some(start), Some(end)) = (
                offset_to_pos(text, name.span().start),
                offset_to_pos(text, name.span().end),
            ) {
                ls_types::Range { start, end }
            } else {
                continue; // Outside the document?
            };

            let (message, severity) = if !versions.is_empty() {
                let (prefix, severity) = if info.req() == "*" {
                    ("Matches any Version", DiagnosticSeverity::INFORMATION)
                } else if let Some(pos) = versions.iter().position(|v| v.starts_with(info.req())) {
                    if pos == 0 {
                        ("Latest Version", DiagnosticSeverity::HINT)
                    } else {
                        ("Outdated Version", DiagnosticSeverity::WARNING)
                    }
                } else {
                    ("Unknown Version", DiagnosticSeverity::ERROR)
                };
                let message = format!(
                    "{prefix}\n\n{} ({})\n{}",
                    name.as_ref(),
                    info.req(),
                    versions.join("\n")
                );

                (message, severity)
            } else {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!(
                            "Failed to fetch versions for {}:\n{:?}",
                            name.as_ref(),
                            info
                        ),
                    )
                    .await;
                (
                    format!("Failed to fetch versions for {}", name.as_ref()),
                    DiagnosticSeverity::ERROR,
                )
            };

            diagnostics.push(ls_types::Diagnostic {
                range,
                severity: Some(severity),
                source: Some("crates-io".into()),
                message,
                ..Default::default()
            });
        }

        Ok(diagnostics)
    }

    pub async fn get_versions(&self, names: Vec<String>) -> Vec<(String, Vec<String>)> {
        let mut set = tokio::task::JoinSet::new();
        let mut results = Vec::new();
        {
            // Read access
            let cache = self.cache.read().await;
            for name in names {
                if let Some(versions) = cache.get(&name) {
                    results.push((name, versions.clone()));
                } else {
                    let endpoint = self.endpoint.clone();
                    let token = self.token.clone();
                    set.spawn(async move {
                        let versions = api::fetch_versions(&name, &endpoint, &token).await;
                        (name, versions)
                    });
                }
            }
        }
        let joined = set.join_all().await;
        if !joined.is_empty() {
            // Lock only if necessary
            let mut cache = self.cache.write().await;
            for (name, versions) in joined {
                match versions {
                    Ok(versions) => {
                        cache.insert(name.clone(), versions.clone());
                        results.push((name, versions));
                    }
                    Err(e) => {
                        self.client
                            .log_message(MessageType::ERROR, format!("Failed fetching {name}: {e}"))
                            .await
                    }
                }
            }
        }
        results
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let (service, socket) = LspService::new(|client| CratesIoBackend {
        client,
        endpoint: args.endpoint,
        token: args.token,
        cache: Default::default(),
        open_docs: Default::default(),
    });

    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
}

fn is_cargo_toml(uri: &Uri) -> bool {
    uri.path()
        .segments_if_absolute()
        .is_some_and(|segments| segments.last().is_some_and(|n| n == "Cargo.toml"))
}

fn pos_to_offset(text: &str, pos: Position) -> Option<usize> {
    let line = text.lines().nth(pos.line as _)?;
    let line_start = text.as_bytes().element_offset(line.as_bytes().first()?)?;
    Some(line_start + pos.character as usize)
}

fn offset_to_pos(text: &str, offset: usize) -> Option<Position> {
    if offset + 1 > text.len() {
        return None;
    }
    // Unfortunate lines does not handle empty last line
    let (line, column) = if text[..offset].ends_with("\n") {
        (text[..offset].lines().count(), 0)
    } else {
        (
            text[..offset].lines().count() - 1,
            text[..offset].lines().last().unwrap().len(),
        )
    };
    Some(Position {
        line: line as _,
        character: column as _,
    })
}

#[derive(Debug, Clone)]
struct FileInfo {
    text: String,
    version: i32,
}
impl FileInfo {
    fn new(text: String, version: i32) -> Self {
        Self { text, version }
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct SpannedManifest {
    dependencies: BTreeMap<Spanned<String>, Dependency>,
    build_dependencies: BTreeMap<Spanned<String>, Dependency>,
    dev_dependencies: BTreeMap<Spanned<String>, Dependency>,
}
