use std::collections::{BTreeMap, HashMap};
use std::thread;

use anyhow::Result;
use cargo_manifest::Dependency;
use clap::Parser;
use lsp_server::{Connection, Message, Notification};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
    Notification as _, PublishDiagnostics,
};
use lsp_types::{
    DiagnosticServerCapabilities, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    InitializeParams, Position, PublishDiagnosticsParams, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
};
use toml::Spanned;
use tracing::{error, info, warn, Level};

mod api;

#[derive(Parser, Debug, Clone)]
struct Args {
    #[arg(short, long, default_value = "https://index.crates.io")]
    endpoint: String,
    #[arg(short, long)]
    token: Option<String>,
}

fn main() -> Result<()> {
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .without_time()
        .with_file(true)
        .with_line_number(true)
        .with_target(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    info!(
        "Starting LSP {:?}",
        std::env::args().skip(1).collect::<Vec<_>>()
    );
    let args = Args::parse();

    let (connection, _io_threads) = Connection::stdio();

    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        diagnostic_provider: Some(DiagnosticServerCapabilities::Options(Default::default())),
        ..Default::default()
    };
    let capabilities = serde_json::to_value(capabilities).unwrap();
    let init_params = connection.initialize(capabilities)?;
    let params: InitializeParams = serde_json::from_value(init_params)?;

    info!("Connected to {:?}", params.client_info);

    let mut open_docs = HashMap::new();

    thread::scope(|scope| {
        let mut version_db = api::VersionDB::new(scope, &args.endpoint, args.token.as_deref());

        for msg in &connection.receiver {
            match msg {
                Message::Request(req) => error!("Unsupported request: {}", req.method),
                Message::Response(resp) => {
                    info!("Received response: {resp:?}");
                }
                Message::Notification(notif) => {
                    handle_notification(notif, &connection, &mut open_docs, &mut version_db)?
                }
            }
        }
        Ok(())
    })
}

fn handle_notification(
    notif: Notification,
    connection: &Connection,
    open_docs: &mut HashMap<Uri, FileInfo>,
    version_db: &mut api::VersionDB,
) -> Result<()> {
    match notif.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let params: DidOpenTextDocumentParams = notif.extract(DidOpenTextDocument::METHOD)?;
            info!(
                "DidOpenTextDocument: {} {}",
                params.text_document.version,
                params.text_document.uri.as_str()
            );
            if !is_cargo_toml(&params.text_document.uri) {
                info!("Skipping {}", params.text_document.uri.as_str());
                return Ok(());
            }

            open_docs.insert(
                params.text_document.uri.clone(),
                FileInfo::new(
                    params.text_document.text.clone(),
                    params.text_document.version,
                ),
            );

            update_diagnostics(
                connection,
                &params.text_document.uri,
                Some(params.text_document.version),
                &params.text_document.text,
                version_db,
            );
        }
        DidChangeTextDocument::METHOD => {
            let params: DidChangeTextDocumentParams =
                notif.extract(DidChangeTextDocument::METHOD)?;
            info!(
                "DidChangeTextDocument: {} {}",
                params.text_document.version,
                params.text_document.uri.as_str()
            );
            if !is_cargo_toml(&params.text_document.uri) {
                info!("Skipping {}", params.text_document.uri.as_str());
                return Ok(());
            }

            let doc = open_docs.get_mut(&params.text_document.uri).unwrap();
            for change in params.content_changes {
                if let Some(range) = change.range {
                    let start = pos_to_offset(&doc.text, range.start).unwrap();
                    let end = pos_to_offset(&doc.text, range.end).unwrap();
                    doc.text.replace_range(start..end, &change.text);
                } else {
                    doc.text = change.text;
                }
            }
            doc.version = params.text_document.version;
        }
        DidSaveTextDocument::METHOD => {
            let params: DidSaveTextDocumentParams = notif.extract(DidSaveTextDocument::METHOD)?;
            info!("DidSaveTextDocument: {}", params.text_document.uri.as_str());
            if !is_cargo_toml(&params.text_document.uri) {
                info!("Skipping {}", params.text_document.uri.as_str());
                return Ok(());
            }

            let doc = open_docs.get_mut(&params.text_document.uri);
            let (text, version) = if let (Some(doc), Some(text)) = (doc, &params.text) {
                doc.text = text.clone();
                (&doc.text, Some(doc.version))
            } else if let Some(text) = &params.text {
                (text, None)
            } else {
                return Ok(());
            };
            update_diagnostics(
                connection,
                &params.text_document.uri,
                version,
                text,
                version_db,
            );
        }
        DidCloseTextDocument::METHOD => {
            let params: DidCloseTextDocumentParams = notif.extract(DidCloseTextDocument::METHOD)?;
            info!(
                "DidCloseTextDocument: {}",
                params.text_document.uri.as_str()
            );
            if !is_cargo_toml(&params.text_document.uri) {
                info!("Skipping {}", params.text_document.uri.as_str());
                return Ok(());
            }

            // Clear diagnostics for the closed document
            publish_diagnostics(connection, &params.text_document.uri, None, Vec::new());
            open_docs.remove(&params.text_document.uri);
        }
        _ => warn!("Unhandled notification: {notif:?}"),
    }
    Ok(())
}

fn is_cargo_toml(uri: &Uri) -> bool {
    uri.path()
        .segments()
        .next_back()
        .is_some_and(|n| n == "Cargo.toml")
}

fn pos_to_offset(text: &str, pos: Position) -> Option<usize> {
    let line = text.lines().nth(pos.line as _)?;
    let line_start = unsafe { line.as_ptr().offset_from(text.as_ptr()) };
    assert!(line_start >= 0);
    Some(line_start as usize + pos.character as usize)
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

fn update_diagnostics(
    connection: &Connection,
    uri: &Uri,
    version: Option<i32>,
    text: &str,
    version_db: &mut api::VersionDB,
) {
    match collect_diagnostics(text, version_db) {
        Ok(diagnostics) => publish_diagnostics(connection, uri, version, diagnostics),
        Err(err) => error!("Failed diagnostics: {err}"),
    }
}

fn publish_diagnostics(
    connection: &Connection,
    uri: &Uri,
    version: Option<i32>,
    diagnostics: Vec<lsp_types::Diagnostic>,
) {
    connection
        .sender
        .send(Message::Notification(Notification::new(
            PublishDiagnostics::METHOD.into(),
            PublishDiagnosticsParams {
                uri: uri.clone(),
                diagnostics,
                version,
            },
        )))
        .unwrap();
}
fn collect_diagnostics(
    text: &str,
    version_db: &mut api::VersionDB,
) -> Result<Vec<lsp_types::Diagnostic>> {
    let parsed: SpannedManifest = toml::from_str(text)?;
    let deps = parsed
        .dependencies
        .iter()
        .chain(parsed.build_dependencies.iter())
        .chain(parsed.dev_dependencies.iter())
        .collect::<Vec<_>>();

    let dep_names = deps
        .iter()
        .map(|(name, _)| name.as_ref().to_string())
        .collect::<Vec<_>>();
    // Fetch versions for dependencies (in parallel)
    let dep_versions = version_db.get_versions(dep_names);

    let mut diagnostics = Vec::new();
    for (name, mut versions) in dep_versions {
        versions.reverse();

        let (name, info) = deps.iter().find(|(n, _)| n.as_ref() == &name).unwrap();

        let range = if let (Some(start), Some(end)) = (
            offset_to_pos(text, name.span().start),
            offset_to_pos(text, name.span().end),
        ) {
            lsp_types::Range { start, end }
        } else {
            continue; // Outside the document?
        };

        let (message, severity) = if !versions.is_empty() {
            let (prefix, severity) =
                if let Some(pos) = versions.iter().position(|v| v.starts_with(info.req())) {
                    if pos == 0 {
                        ("Latest Version", DiagnosticSeverity::INFORMATION)
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
            (
                format!("Failed to fetch versions for {}", name.as_ref()),
                DiagnosticSeverity::ERROR,
            )
        };

        diagnostics.push(lsp_types::Diagnostic {
            range,
            severity: Some(severity),
            source: Some("crates-io".to_string()),
            message,
            ..Default::default()
        });
    }

    Ok(diagnostics)
}
