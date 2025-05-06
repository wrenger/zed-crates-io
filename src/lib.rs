use zed::settings::LspSettings;
use zed_extension_api as zed;

struct CratesIoExtension {
    cached_binary: Option<String>,
}

impl zed::Extension for CratesIoExtension {
    fn new() -> Self {
        Self { cached_binary: None }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        println!("Language server command called for {language_server_id}");

        let settings = LspSettings::for_worktree("crates-io", worktree);
        println!("Settings: {settings:?}");

        let binary_settings = settings.ok().and_then(|lsp_settings| lsp_settings.binary);
        let args = binary_settings
            .as_ref()
            .and_then(|settings| settings.arguments.clone())
            .unwrap_or_default();

        if let Some(path) = binary_settings.and_then(|settings| settings.path) {
            return Ok(zed::Command {
                command: path,
                args,
                env: vec![],
            });
        }
        if let Some(path) = worktree.which("crates-io-lsp") {
            return Ok(zed::Command {
                command: path,
                args,
                env: vec![],
            });
        }
        if let Some(path) = self.cached_binary.as_ref() {
            return Ok(zed::Command {
                command: path.clone(),
                args,
                env: vec![],
            });
        }

        // TODO: Implement automatic installation
        Err("Only manual installation is supported".into())
    }
}

zed::register_extension!(CratesIoExtension);
