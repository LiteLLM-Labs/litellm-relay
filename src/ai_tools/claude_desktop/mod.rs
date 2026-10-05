use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::config::{load_settings, save_settings, DesktopSso, RelaySettings};

const MANAGED_SETTINGS_PATH_ENV: &str = "CLAUDE_DESKTOP_MANAGED_SETTINGS";
const MACOS_MANAGED_PLIST: &str =
    "/Library/Managed Preferences/com.anthropic.claudefordesktop.plist";
const LINUX_MANAGED_JSON: &str = "/etc/claude-desktop/managed-settings.json";

/// Inputs for wiring Claude Desktop (third-party mode) to route through the
/// Gateway. Supplied by the MDM package or interactively; any field left unset
/// falls back to the saved Relay config.
///
/// When `oidc_client_id` and `oidc_issuer` are both set, the app is configured
/// for single sign-on: each developer signs in against the corporate IdP and
/// the resulting token is sent to the Gateway as the bearer credential, so no
/// provider key ever lands on the device. Otherwise a static Gateway key is
/// written.
#[derive(Debug, Default)]
pub struct OnboardDesktopParams {
    pub gateway_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_issuer: Option<String>,
    pub oidc_scopes: Option<String>,
    pub oidc_redirect_port: Option<u16>,
    /// Suppress success output (used by autoconfigure, which prints its own
    /// summary). Standalone `relay onboard-claude-desktop` leaves this false.
    pub quiet: bool,
    /// Reuse the OIDC settings saved by an earlier onboard when no OIDC flags
    /// are passed. Autoconfigure sets this so unattended reruns keep whatever
    /// sign-in mode the device was enrolled with; the standalone command
    /// leaves it false so its flags decide.
    pub reuse_saved_sso: bool,
}

/// Writes the managed configuration Claude Desktop reads on launch so it routes
/// inference through the Gateway: the `com.anthropic.claudefordesktop` managed
/// preferences plist on macOS, `/etc/claude-desktop/managed-settings.json` on
/// Linux (see the Anthropic "LLM gateway" third-party docs). The app switches
/// into gateway mode and — in SSO mode — prompts the developer to sign in
/// through their browser on first use.
pub fn onboard_desktop(params: OnboardDesktopParams) -> Result<()> {
    let mut settings = load_settings()?;
    if let Some(gateway_url) = params.gateway_url {
        settings.gateway.url = gateway_url.trim_end_matches('/').to_string();
    }
    if let Some(api_key) = params.api_key {
        settings.gateway.enroll_if_changed(api_key);
    }
    if let Some(model) = params.model {
        settings.claude.model = model;
    }

    let sso = match (&params.oidc_client_id, &params.oidc_issuer) {
        (Some(client_id), Some(issuer)) => Some(DesktopSso {
            client_id: client_id.clone(),
            issuer: issuer.clone(),
            scopes: params.oidc_scopes.clone(),
            redirect_port: params.oidc_redirect_port,
        }),
        (None, None) if params.reuse_saved_sso => settings.claude.desktop_sso.clone(),
        (None, None) => None,
        _ => bail!("SSO requires both --oidc-client-id and --oidc-issuer"),
    };
    settings.claude.desktop_sso = sso.clone();

    if sso.is_none() && settings.gateway.api_key.as_deref().unwrap_or("").is_empty() {
        bail!(
            "Claude Desktop onboarding needs a Gateway credential: pass --api-key for a static \
             key, or --oidc-client-id and --oidc-issuer for single sign-on"
        );
    }

    let document = build_managed_settings(&settings, sso.as_ref());
    let written = write_managed_settings(&ManagedLayout::for_host(), &document)?;
    save_settings(&settings)?;

    if let Some(stale) = &written.removed_stale {
        println!("Removed stale {}", stale.display());
    }
    if !params.quiet {
        println!("Claude Desktop is wired to {}", settings.gateway.url);
        match &sso {
            Some(sso) => {
                println!(
                    "Sign-in: OIDC issuer {} (client {})",
                    sso.issuer, sso.client_id
                );
                println!("Developers click \"Sign in to your organization\" on first launch.");
            }
            None => println!("Credential: static Gateway API key"),
        }
        println!("Wrote {}", written.path.display());
        println!("Restart Claude Desktop to pick up the managed configuration.");
    }
    Ok(())
}

/// Builds the top-level object Claude Desktop reads from its managed
/// configuration. Keys match the Anthropic third-party configuration reference
/// exactly; the same document is rendered as a plist on macOS and as JSON on
/// Linux.
fn build_managed_settings(
    settings: &RelaySettings,
    sso: Option<&DesktopSso>,
) -> Map<String, Value> {
    let mut root = Map::new();
    root.insert("inferenceProvider".into(), Value::String("gateway".into()));
    root.insert(
        "inferenceGatewayBaseUrl".into(),
        Value::String(settings.gateway.url.clone()),
    );
    root.insert(
        "inferenceGatewayAuthScheme".into(),
        Value::String("bearer".into()),
    );
    root.insert(
        "inferenceModels".into(),
        Value::Array(vec![Value::String(settings.claude.model.clone())]),
    );

    match sso {
        Some(sso) => {
            root.insert(
                "inferenceCredentialKind".into(),
                Value::String("interactive".into()),
            );
            let mut oidc = Map::new();
            oidc.insert("clientId".into(), Value::String(sso.client_id.clone()));
            oidc.insert("issuer".into(), Value::String(sso.issuer.clone()));
            if let Some(scopes) = &sso.scopes {
                oidc.insert("scopes".into(), Value::String(scopes.clone()));
            }
            if let Some(port) = sso.redirect_port {
                oidc.insert("redirectPort".into(), json!(port));
            }
            root.insert("inferenceGatewayOidc".into(), Value::Object(oidc));
        }
        None => {
            root.insert(
                "inferenceCredentialKind".into(),
                Value::String("static".into()),
            );
            if let Some(api_key) = &settings.gateway.api_key {
                root.insert(
                    "inferenceGatewayApiKey".into(),
                    Value::String(api_key.clone()),
                );
            }
        }
    }

    root
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedFormat {
    MacOsPlist,
    LinuxJson,
}

impl ManagedFormat {
    fn for_host() -> Self {
        if cfg!(target_os = "macos") {
            ManagedFormat::MacOsPlist
        } else {
            ManagedFormat::LinuxJson
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedLayout {
    path: PathBuf,
    format: ManagedFormat,
    stale_path: Option<PathBuf>,
}

impl ManagedLayout {
    fn for_host() -> Self {
        Self::resolve(env::var_os(MANAGED_SETTINGS_PATH_ENV).map(PathBuf::from))
    }

    fn resolve(path_override: Option<PathBuf>) -> Self {
        if let Some(path) = path_override {
            let format = match path.extension().and_then(|ext| ext.to_str()) {
                Some(ext) if ext.eq_ignore_ascii_case("plist") => ManagedFormat::MacOsPlist,
                _ => ManagedFormat::LinuxJson,
            };
            return Self {
                path,
                format,
                stale_path: None,
            };
        }
        let format = ManagedFormat::for_host();
        match format {
            ManagedFormat::MacOsPlist => Self {
                path: PathBuf::from(MACOS_MANAGED_PLIST),
                format,
                stale_path: Some(PathBuf::from(LINUX_MANAGED_JSON)),
            },
            ManagedFormat::LinuxJson => Self {
                path: PathBuf::from(LINUX_MANAGED_JSON),
                format,
                stale_path: None,
            },
        }
    }

    fn managed_dir(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("/"))
    }
}

#[derive(Debug)]
struct ManagedWrite {
    path: PathBuf,
    removed_stale: Option<PathBuf>,
}

fn write_managed_settings(
    layout: &ManagedLayout,
    document: &Map<String, Value>,
) -> Result<ManagedWrite> {
    if let Some(per_user) = find_per_user_override(layout, document) {
        bail!(
            "per-user managed plist {} sets {}, and Claude Desktop reads it over {}",
            per_user.path.display(),
            per_user.keys.join(", "),
            layout.path.display()
        );
    }

    if verify_managed_settings(layout, document).is_err() {
        let managed_dir = layout.managed_dir();
        let dir_missing = !managed_dir.exists();
        fs::create_dir_all(managed_dir)
            .map_err(|error| managed_write_error(error, layout, managed_dir))?;
        pin_managed_dir_mode(managed_dir, dir_missing)
            .map_err(|error| managed_write_error(error, layout, managed_dir))?;
        let rendered = render_managed_settings(layout.format, document)?;
        replace_atomically(&layout.path, &rendered)
            .map_err(|error| managed_write_error(error, layout, &layout.path))?;
        verify_managed_settings(layout, document)?;
    }

    let removed_stale = match &layout.stale_path {
        Some(stale) => remove_stale_managed_file(stale, layout)?,
        None => None,
    };
    Ok(ManagedWrite {
        path: layout.path.clone(),
        removed_stale,
    })
}

struct PerUserOverride {
    path: PathBuf,
    keys: Vec<String>,
}

fn find_per_user_override(
    layout: &ManagedLayout,
    document: &Map<String, Value>,
) -> Option<PerUserOverride> {
    if layout.format != ManagedFormat::MacOsPlist {
        return None;
    }
    let file_name = layout.path.file_name()?;
    fs::read_dir(layout.managed_dir())
        .ok()?
        .flatten()
        .map(|account_dir| account_dir.path().join(file_name))
        .find_map(|path| {
            let per_user: plist::Value = plist::from_file(&path).ok()?;
            let keys: Vec<String> = per_user
                .as_dictionary()?
                .keys()
                .filter(|key| document.contains_key(*key))
                .cloned()
                .collect();
            (!keys.is_empty()).then_some(PerUserOverride { path, keys })
        })
}

fn replace_atomically(path: &Path, rendered: &[u8]) -> io::Result<()> {
    let staged = path.with_extension("relay-tmp");
    fs::write(&staged, rendered)?;
    mark_world_readable(&staged)
        .and_then(|()| fs::rename(&staged, path))
        .inspect_err(|_| {
            let _ = fs::remove_file(&staged);
        })
}

#[cfg(unix)]
fn mark_world_readable(staged: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(staged, fs::Permissions::from_mode(0o644))
}

#[cfg(not(unix))]
fn mark_world_readable(_staged: &Path) -> io::Result<()> {
    Ok(())
}

/// A managed directory Relay creates is pinned to 0755 so the tool's managed
/// configuration stays readable to every local account regardless of umask.
/// Directories that already existed keep whatever mode the admin set.
#[cfg(unix)]
fn pin_managed_dir_mode(dir: &Path, created: bool) -> io::Result<()> {
    if created {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn pin_managed_dir_mode(_dir: &Path, _created: bool) -> io::Result<()> {
    Ok(())
}

fn render_managed_settings(
    format: ManagedFormat,
    document: &Map<String, Value>,
) -> Result<Vec<u8>> {
    match format {
        ManagedFormat::MacOsPlist => {
            let mut rendered = Vec::new();
            plist::to_writer_xml(&mut rendered, document)
                .context("failed to render the managed plist")?;
            Ok(rendered)
        }
        ManagedFormat::LinuxJson => {
            let rendered = serde_json::to_string_pretty(&Value::Object(document.clone()))?;
            Ok(format!("{rendered}\n").into_bytes())
        }
    }
}

fn verify_managed_settings(layout: &ManagedLayout, document: &Map<String, Value>) -> Result<()> {
    let matches = match layout.format {
        ManagedFormat::MacOsPlist => {
            let on_disk: plist::Value = plist::from_file(&layout.path)
                .with_context(|| format!("failed to read back {}", layout.path.display()))?;
            on_disk == plist::to_value(document).context("failed to render the managed plist")?
        }
        ManagedFormat::LinuxJson => {
            let contents = fs::read(&layout.path)
                .with_context(|| format!("failed to read back {}", layout.path.display()))?;
            let on_disk: Value = serde_json::from_slice(&contents)
                .with_context(|| format!("failed to read back {}", layout.path.display()))?;
            on_disk == Value::Object(document.clone())
        }
    };
    if !matches {
        bail!(
            "{} does not contain the managed settings just written, so Claude Desktop cannot pick them up",
            layout.path.display()
        );
    }
    Ok(())
}

fn remove_stale_managed_file(stale: &Path, layout: &ManagedLayout) -> Result<Option<PathBuf>> {
    if !holds_gateway_settings(stale) {
        return Ok(None);
    }
    fs::remove_file(stale).map_err(|error| managed_write_error(error, layout, stale))?;
    remove_dir_if_empty(stale.parent());
    Ok(Some(stale.to_path_buf()))
}

fn holds_gateway_settings(path: &Path) -> bool {
    fs::read(path)
        .ok()
        .and_then(|contents| serde_json::from_slice::<Value>(&contents).ok())
        .is_some_and(|settings| settings["inferenceProvider"] == "gateway")
}

fn remove_dir_if_empty(dir: Option<&Path>) {
    if let Some(dir) = dir {
        let _ = fs::remove_dir(dir);
    }
}

/// Maps a filesystem error on the managed location to a concise, actionable
/// message. Permission errors get a short "needs sudo" hint (surfaced verbatim
/// in the autoconfigure summary) instead of the raw
/// "Permission denied (os error 13)".
fn managed_write_error(error: io::Error, layout: &ManagedLayout, path: &Path) -> anyhow::Error {
    if error.kind() == io::ErrorKind::PermissionDenied {
        anyhow!(
            "needs sudo (managed dir {} is root-owned)",
            layout.managed_dir().display()
        )
    } else {
        anyhow::Error::new(error).context(format!("failed to write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(url: &str, key: Option<&str>, model: &str) -> RelaySettings {
        let mut settings = RelaySettings::default();
        settings.gateway.url = url.into();
        settings.gateway.api_key = key.map(str::to_string);
        settings.claude.model = model.into();
        settings
    }

    fn sso_config() -> DesktopSso {
        DesktopSso {
            client_id: "client-123".into(),
            issuer: "https://login.corp/v2.0".into(),
            scopes: None,
            redirect_port: Some(53180),
        }
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("relay-desktop-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn layout_in(
        dir: &Path,
        file: &str,
        format: ManagedFormat,
        stale_path: Option<PathBuf>,
    ) -> ManagedLayout {
        ManagedLayout {
            path: dir.join("managed").join(file),
            format,
            stale_path,
        }
    }

    #[test]
    fn should_write_static_gateway_config() {
        let settings = settings_with(
            "http://127.0.0.1:4000",
            Some("sk-test"),
            "claude-sonnet-4-5",
        );
        let doc = build_managed_settings(&settings, None);

        assert_eq!(doc["inferenceProvider"], Value::String("gateway".into()));
        assert_eq!(
            doc["inferenceGatewayBaseUrl"],
            Value::String("http://127.0.0.1:4000".into())
        );
        assert_eq!(
            doc["inferenceCredentialKind"],
            Value::String("static".into())
        );
        assert_eq!(
            doc["inferenceGatewayApiKey"],
            Value::String("sk-test".into())
        );
        assert!(!doc.contains_key("inferenceGatewayOidc"));
        assert_eq!(doc["inferenceModels"], json!(["claude-sonnet-4-5"]));
    }

    #[test]
    fn should_write_interactive_sso_config_without_api_key() {
        let settings = settings_with("https://gw.corp", Some("sk-secret"), "claude-sonnet-4-5");
        let doc = build_managed_settings(&settings, Some(&sso_config()));

        assert_eq!(
            doc["inferenceCredentialKind"],
            Value::String("interactive".into())
        );
        assert!(
            !doc.contains_key("inferenceGatewayApiKey"),
            "SSO mode must not leak a static key onto the device"
        );
        let oidc = doc["inferenceGatewayOidc"].as_object().unwrap();
        assert_eq!(oidc["clientId"], Value::String("client-123".into()));
        assert_eq!(
            oidc["issuer"],
            Value::String("https://login.corp/v2.0".into())
        );
        assert_eq!(oidc["redirectPort"], json!(53180));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn should_target_the_managed_preferences_plist_on_macos() {
        let layout = ManagedLayout::resolve(None);

        assert_eq!(
            layout.path,
            PathBuf::from("/Library/Managed Preferences/com.anthropic.claudefordesktop.plist")
        );
        assert_eq!(layout.format, ManagedFormat::MacOsPlist);
        assert_eq!(
            layout.stale_path.as_deref(),
            Some(Path::new("/etc/claude-desktop/managed-settings.json")),
            "the /etc file earlier Relay versions wrote on macOS must be cleaned up"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn should_target_the_etc_json_file_on_linux() {
        let layout = ManagedLayout::resolve(None);

        assert_eq!(
            layout.path,
            PathBuf::from("/etc/claude-desktop/managed-settings.json")
        );
        assert_eq!(layout.format, ManagedFormat::LinuxJson);
        assert_eq!(layout.stale_path, None);
    }

    #[test]
    fn should_honor_the_path_override_and_skip_the_stale_cleanup() {
        let layout = ManagedLayout::resolve(Some(PathBuf::from("/tmp/relay/managed.plist")));

        assert_eq!(layout.path, PathBuf::from("/tmp/relay/managed.plist"));
        assert_eq!(layout.format, ManagedFormat::MacOsPlist);
        assert_eq!(
            layout.stale_path, None,
            "a custom layout must never delete the real /etc file"
        );
    }

    #[test]
    fn should_pick_the_override_format_from_the_path_extension() {
        let json = ManagedLayout::resolve(Some(PathBuf::from("/tmp/relay/managed.json")));
        assert_eq!(json.format, ManagedFormat::LinuxJson);

        let plist = ManagedLayout::resolve(Some(PathBuf::from("/tmp/relay/managed.PLIST")));
        assert_eq!(plist.format, ManagedFormat::MacOsPlist);
    }

    #[test]
    fn should_write_a_plist_claude_desktop_can_parse_including_nested_oidc() {
        let dir = scratch_dir("plist");
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            None,
        );
        let settings = settings_with("https://gw.corp", None, "claude-sonnet-5");
        let doc = build_managed_settings(&settings, Some(&sso_config()));

        let written = write_managed_settings(&layout, &doc).unwrap();

        assert_eq!(written.path, layout.path);
        let on_disk: plist::Value = plist::from_file(&layout.path).unwrap();
        let root = on_disk.as_dictionary().expect("plist root must be a dict");
        assert_eq!(
            root.get("inferenceGatewayBaseUrl")
                .and_then(plist::Value::as_string),
            Some("https://gw.corp")
        );
        assert_eq!(
            root.get("inferenceCredentialKind")
                .and_then(plist::Value::as_string),
            Some("interactive")
        );
        let models = root
            .get("inferenceModels")
            .and_then(plist::Value::as_array)
            .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].as_string(), Some("claude-sonnet-5"));
        let oidc = root
            .get("inferenceGatewayOidc")
            .and_then(plist::Value::as_dictionary)
            .unwrap();
        assert_eq!(
            oidc.get("clientId").and_then(plist::Value::as_string),
            Some("client-123")
        );
        assert_eq!(
            oidc.get("redirectPort")
                .and_then(plist::Value::as_unsigned_integer),
            Some(53180),
            "the redirect port must stay an integer, not become a string"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_write_json_on_the_linux_layout() {
        let dir = scratch_dir("json");
        let layout = layout_in(
            &dir,
            "managed-settings.json",
            ManagedFormat::LinuxJson,
            None,
        );
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);

        write_managed_settings(&layout, &doc).unwrap();

        let on_disk: Value = serde_json::from_slice(&fs::read(&layout.path).unwrap()).unwrap();
        assert_eq!(on_disk, Value::Object(doc));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_remove_the_stale_etc_file_and_its_dir_after_a_verified_write() {
        let dir = scratch_dir("stale");
        let stale = dir
            .join("etc")
            .join("claude-desktop")
            .join("managed-settings.json");
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(
            &stale,
            "{\"inferenceProvider\": \"gateway\", \"inferenceGatewayApiKey\": \"sk-old\"}\n",
        )
        .unwrap();
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            Some(stale.clone()),
        );
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);

        let written = write_managed_settings(&layout, &doc).unwrap();

        assert_eq!(written.removed_stale, Some(stale.clone()));
        assert!(!stale.exists(), "the stale credential file must be gone");
        assert!(
            !stale.parent().unwrap().exists(),
            "the emptied /etc/claude-desktop dir must be gone too"
        );
        assert!(layout.path.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_report_nothing_removed_when_no_stale_file_exists() {
        let dir = scratch_dir("nostale");
        let stale = dir
            .join("etc")
            .join("claude-desktop")
            .join("managed-settings.json");
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            Some(stale),
        );
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);

        let written = write_managed_settings(&layout, &doc).unwrap();

        assert_eq!(written.removed_stale, None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_leave_a_stale_path_alone_when_it_is_not_a_gateway_config() {
        let dir = scratch_dir("foreign-stale");
        let stale = dir
            .join("etc")
            .join("claude-desktop")
            .join("managed-settings.json");
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(&stale, "{\"inferenceProvider\": \"anthropic\"}\n").unwrap();
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            Some(stale.clone()),
        );
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);

        let written = write_managed_settings(&layout, &doc).unwrap();

        assert_eq!(written.removed_stale, None);
        assert!(stale.exists(), "a file Relay did not write must survive");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_fail_before_writing_when_a_per_user_plist_overrides_the_gateway_keys() {
        let dir = scratch_dir("per-user");
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            None,
        );
        let per_user = dir
            .join("managed")
            .join("alice")
            .join("com.anthropic.claudefordesktop.plist");
        fs::create_dir_all(per_user.parent().unwrap()).unwrap();
        let mdm_settings = build_managed_settings(
            &settings_with("https://mdm.corp", Some("sk-mdm"), "claude-sonnet-5"),
            None,
        );
        fs::write(
            &per_user,
            render_managed_settings(ManagedFormat::MacOsPlist, &mdm_settings).unwrap(),
        )
        .unwrap();
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);

        let error = write_managed_settings(&layout, &doc)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("alice/com.anthropic.claudefordesktop.plist"),
            "{error}"
        );
        assert!(error.contains("inferenceGatewayBaseUrl"), "{error}");
        assert!(
            !layout.path.exists(),
            "nothing may be written when the settings cannot take effect"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_write_when_a_per_user_plist_sets_unrelated_keys_only() {
        let dir = scratch_dir("per-user-unrelated");
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            None,
        );
        let per_user = dir
            .join("managed")
            .join("alice")
            .join("com.anthropic.claudefordesktop.plist");
        fs::create_dir_all(per_user.parent().unwrap()).unwrap();
        let mut unrelated = Map::new();
        unrelated.insert("mcpEnabled".into(), Value::Bool(false));
        fs::write(
            &per_user,
            render_managed_settings(ManagedFormat::MacOsPlist, &unrelated).unwrap(),
        )
        .unwrap();
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);

        write_managed_settings(&layout, &doc).unwrap();

        assert!(layout.path.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_replace_a_different_file_and_leave_no_staging_file_behind() {
        let dir = scratch_dir("replace");
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            None,
        );
        fs::create_dir_all(layout.managed_dir()).unwrap();
        fs::write(&layout.path, b"not a plist").unwrap();
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);

        write_managed_settings(&layout, &doc).unwrap();

        let on_disk: plist::Value = plist::from_file(&layout.path).unwrap();
        assert_eq!(on_disk, plist::to_value(&doc).unwrap());
        let leftovers: Vec<_> = fs::read_dir(layout.managed_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            leftovers,
            vec![std::ffi::OsString::from(
                "com.anthropic.claudefordesktop.plist"
            )]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn should_leave_the_managed_file_world_readable_whatever_mode_the_staging_file_had() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch_dir("mode");
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            None,
        );
        fs::create_dir_all(layout.managed_dir()).unwrap();
        let staged = layout.path.with_extension("relay-tmp");
        fs::write(&staged, b"").unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o600)).unwrap();
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);

        write_managed_settings(&layout, &doc).unwrap();

        assert_eq!(
            fs::metadata(&layout.path).unwrap().permissions().mode() & 0o777,
            0o644,
            "Claude Desktop reads the root-owned plist as the signed-in user"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn should_not_rewrite_a_file_that_already_holds_the_document() {
        use std::os::unix::fs::MetadataExt;

        let dir = scratch_dir("steady");
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            None,
        );
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);
        write_managed_settings(&layout, &doc).unwrap();
        let first_inode = fs::metadata(&layout.path).unwrap().ino();

        write_managed_settings(&layout, &doc).unwrap();

        assert_eq!(
            fs::metadata(&layout.path).unwrap().ino(),
            first_inode,
            "an hourly re-run must not churn a file that is already current"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_fail_when_the_file_on_disk_is_not_the_document_written() {
        let dir = scratch_dir("mismatch");
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            None,
        );
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);
        let other = build_managed_settings(
            &settings_with("https://other.corp", Some("sk-test"), "claude-sonnet-5"),
            None,
        );
        fs::create_dir_all(layout.managed_dir()).unwrap();
        fs::write(
            &layout.path,
            render_managed_settings(ManagedFormat::MacOsPlist, &other).unwrap(),
        )
        .unwrap();

        let error = verify_managed_settings(&layout, &doc)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("com.anthropic.claudefordesktop.plist"),
            "{error}"
        );
        assert!(error.contains("cannot pick them up"), "{error}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_fail_when_the_file_on_disk_is_not_a_plist() {
        let dir = scratch_dir("garbage");
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            None,
        );
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);
        fs::create_dir_all(layout.managed_dir()).unwrap();
        fs::write(&layout.path, b"{\"inferenceProvider\": \"gateway\"}\n").unwrap();

        let error = verify_managed_settings(&layout, &doc)
            .unwrap_err()
            .to_string();

        assert!(error.contains("failed to read back"), "{error}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_name_the_managed_dir_in_the_needs_sudo_hint() {
        let layout = ManagedLayout {
            path: PathBuf::from(MACOS_MANAGED_PLIST),
            format: ManagedFormat::MacOsPlist,
            stale_path: None,
        };

        let error = managed_write_error(
            io::Error::from(io::ErrorKind::PermissionDenied),
            &layout,
            &layout.path,
        )
        .to_string();

        assert_eq!(
            error,
            "needs sudo (managed dir /Library/Managed Preferences is root-owned)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn should_pin_a_managed_dir_it_created_to_0755() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch_dir("dirmode");
        let layout = layout_in(
            &dir,
            "com.anthropic.claudefordesktop.plist",
            ManagedFormat::MacOsPlist,
            None,
        );
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-5");
        let doc = build_managed_settings(&settings, None);

        write_managed_settings(&layout, &doc).unwrap();

        assert_eq!(
            fs::metadata(layout.managed_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "a managed dir Relay creates must stay readable to every local account"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(value) => env::set_var(name, value),
            None => env::remove_var(name),
        }
    }

    #[test]
    fn should_reuse_the_saved_sso_on_an_unattended_rerun() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = scratch_dir("sso-reuse-home");
        let managed = scratch_dir("sso-reuse").join("managed.plist");
        let old_home = env::var_os("HOME");
        let old_override = env::var_os(MANAGED_SETTINGS_PATH_ENV);
        env::set_var("HOME", &home);
        env::set_var(MANAGED_SETTINGS_PATH_ENV, &managed);

        onboard_desktop(OnboardDesktopParams {
            gateway_url: Some("https://gw.corp".into()),
            api_key: Some("sk-saved".into()),
            oidc_client_id: Some("client-1".into()),
            oidc_issuer: Some("https://issuer.corp".into()),
            quiet: true,
            ..OnboardDesktopParams::default()
        })
        .unwrap();
        onboard_desktop(OnboardDesktopParams {
            reuse_saved_sso: true,
            quiet: true,
            ..OnboardDesktopParams::default()
        })
        .unwrap();

        let on_disk: plist::Value = plist::from_file(&managed).unwrap();
        assert_eq!(
            on_disk.as_dictionary().unwrap()["inferenceCredentialKind"],
            plist::Value::String("interactive".into()),
            "the daemon rerun must rebuild the SSO document, not fall back to the saved key"
        );
        let saved = load_settings().unwrap();
        assert_eq!(
            saved
                .claude
                .desktop_sso
                .as_ref()
                .map(|sso| sso.client_id.as_str()),
            Some("client-1")
        );

        restore_env("HOME", old_home);
        restore_env(MANAGED_SETTINGS_PATH_ENV, old_override);
        fs::remove_dir_all(&home).unwrap();
        fs::remove_dir_all(managed.parent().unwrap()).unwrap();
    }

    #[test]
    fn should_keep_the_setup_enrollment_timestamps_on_an_unattended_rerun() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = scratch_dir("keep-expiry-home");
        let managed = scratch_dir("keep-expiry").join("managed.plist");
        let old_home = env::var_os("HOME");
        let old_override = env::var_os(MANAGED_SETTINGS_PATH_ENV);
        env::set_var("HOME", &home);
        env::set_var(MANAGED_SETTINGS_PATH_ENV, &managed);
        let enrolled_at = chrono::DateTime::parse_from_rfc3339("2026-09-21T21:27:58Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let expires_at = chrono::DateTime::parse_from_rfc3339("2026-09-23T21:27:58Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut enrolled = settings_with("https://gw.corp", Some("sk-saved"), "claude-sonnet-4-5");
        enrolled.gateway.enrolled_at = Some(enrolled_at);
        enrolled.gateway.expires_at = Some(expires_at);
        save_settings(&enrolled).unwrap();

        onboard_desktop(OnboardDesktopParams {
            api_key: Some("sk-saved".into()),
            reuse_saved_sso: true,
            quiet: true,
            ..OnboardDesktopParams::default()
        })
        .unwrap();

        let saved = load_settings().unwrap();
        assert_eq!(saved.gateway.api_key.as_deref(), Some("sk-saved"));
        assert_eq!(
            saved.gateway.enrolled_at,
            Some(enrolled_at),
            "handing the saved key back must not move the enrollment stamp"
        );
        assert_eq!(
            saved.gateway.expires_at,
            Some(expires_at),
            "handing the saved key back must keep the expiry setup recorded"
        );

        restore_env("HOME", old_home);
        restore_env(MANAGED_SETTINGS_PATH_ENV, old_override);
        fs::remove_dir_all(&home).unwrap();
        fs::remove_dir_all(managed.parent().unwrap()).unwrap();
    }

    #[test]
    fn should_switch_back_to_a_static_key_and_clear_the_saved_sso() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = scratch_dir("sso-clear-home");
        let managed = scratch_dir("sso-clear").join("managed.plist");
        let old_home = env::var_os("HOME");
        let old_override = env::var_os(MANAGED_SETTINGS_PATH_ENV);
        env::set_var("HOME", &home);
        env::set_var(MANAGED_SETTINGS_PATH_ENV, &managed);

        onboard_desktop(OnboardDesktopParams {
            gateway_url: Some("https://gw.corp".into()),
            api_key: Some("sk-saved".into()),
            oidc_client_id: Some("client-1".into()),
            oidc_issuer: Some("https://issuer.corp".into()),
            quiet: true,
            ..OnboardDesktopParams::default()
        })
        .unwrap();
        onboard_desktop(OnboardDesktopParams {
            api_key: Some("sk-saved".into()),
            reuse_saved_sso: false,
            quiet: true,
            ..OnboardDesktopParams::default()
        })
        .unwrap();

        let on_disk: plist::Value = plist::from_file(&managed).unwrap();
        assert_eq!(
            on_disk.as_dictionary().unwrap()["inferenceCredentialKind"],
            plist::Value::String("static".into())
        );
        let saved = load_settings().unwrap();
        assert!(
            saved.claude.desktop_sso.is_none(),
            "a standalone static onboard must clear the saved SSO"
        );

        restore_env("HOME", old_home);
        restore_env(MANAGED_SETTINGS_PATH_ENV, old_override);
        fs::remove_dir_all(&home).unwrap();
        fs::remove_dir_all(managed.parent().unwrap()).unwrap();
    }
}
