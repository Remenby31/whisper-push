//! Register (and unregister) Whisper Push with every MCP client on the machine.
//!
//! A `speak` tool nobody wired up is a tool nobody uses, so this does the wiring
//! — but it edits *other applications'* configuration files, which demands some
//! restraint:
//!
//! - only ever touch our own entry, never rewrite the rest of a file;
//! - back the file up once before the first modification;
//! - write atomically (temp + rename) so a crash can't truncate someone's config;
//! - be idempotent — re-running updates the binary path instead of duplicating;
//! - never create a config for a client that isn't installed.
//!
//! Codex's config is TOML that users hand-edit, so it goes through `toml_edit`,
//! which preserves comments and layout. Plain `toml` would silently reformat the
//! whole file.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The key we own in every client's config.
const SERVER_NAME: &str = "whisper-push";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Format {
    /// `{"mcpServers": {"<name>": {"command":…, "args":[…]}}}`
    Json,
    /// `[mcp_servers.<name>]` with `command` / `args`.
    Toml,
}

pub struct Client {
    pub id: &'static str,
    pub label: &'static str,
    path: PathBuf,
    format: Format,
    /// Clients that only re-read their config on restart.
    pub needs_restart: bool,
}

impl Client {
    /// Present = the config file exists. We deliberately do not create configs
    /// for absent apps: an empty `~/.cursor/mcp.json` would be litter.
    pub fn installed(&self) -> bool {
        self.path.exists()
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Every client we know how to write to.
///
/// Cowork is deliberately absent: its config location is not something we can
/// confirm, and guessing a path that gets written to disk is not acceptable.
pub fn known_clients() -> Vec<Client> {
    let h = home();
    vec![
        Client {
            id: "claude-code",
            label: "Claude Code",
            path: h.join(".claude.json"),
            format: Format::Json,
            needs_restart: false,
        },
        Client {
            id: "mcp-json",
            label: "Global ~/.mcp.json",
            path: h.join(".mcp.json"),
            format: Format::Json,
            needs_restart: false,
        },
        Client {
            id: "codex",
            label: "Codex",
            path: h.join(".codex/config.toml"),
            format: Format::Toml,
            needs_restart: true,
        },
        Client {
            id: "claude-desktop",
            label: "Claude Desktop",
            path: h.join("Library/Application Support/Claude/claude_desktop_config.json"),
            format: Format::Json,
            needs_restart: true,
        },
        Client {
            id: "cursor",
            label: "Cursor",
            path: h.join(".cursor/mcp.json"),
            format: Format::Json,
            needs_restart: true,
        },
        Client {
            id: "windsurf",
            label: "Windsurf",
            path: h.join(".codeium/windsurf/mcp_config.json"),
            format: Format::Json,
            needs_restart: true,
        },
        Client {
            id: "vscode",
            label: "VS Code",
            path: h.join("Library/Application Support/Code/User/mcp.json"),
            format: Format::Json,
            needs_restart: true,
        },
    ]
}

/// Absolute path to the running binary — what the client must invoke. Resolving
/// symlinks matters: a client launched outside the shell has no PATH of ours.
fn binary_path() -> Result<String> {
    let exe = std::env::current_exe().context("Cannot determine our own path")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Ok(exe.to_string_lossy().into_owned())
}

/// Is our entry actually registered in this client's config?
///
/// Parsed, not grepped: the literal string "whisper-push" shows up in unrelated
/// places (a project path, another server's command) and a substring match
/// reports a false ✓.
pub fn is_registered(client: &Client) -> bool {
    let Ok(raw) = std::fs::read_to_string(client.path()) else {
        return false;
    };
    match client.format {
        Format::Json => serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| v.get("mcpServers")?.get(SERVER_NAME).cloned())
            .is_some(),
        Format::Toml => raw
            .parse::<toml_edit::DocumentMut>()
            .ok()
            .and_then(|d| Some(d.get("mcp_servers")?.get(SERVER_NAME).is_some()))
            .unwrap_or(false),
    }
}

pub struct Outcome {
    pub label: &'static str,
    pub path: PathBuf,
    pub changed: bool,
    pub needs_restart: bool,
}

/// Register with every installed client (or just those whose id is in `only`).
pub fn install(only: &[String], dry_run: bool) -> Result<Vec<Outcome>> {
    apply(only, dry_run, true)
}

/// Remove our entry everywhere.
pub fn uninstall(only: &[String], dry_run: bool) -> Result<Vec<Outcome>> {
    apply(only, dry_run, false)
}

fn apply(only: &[String], dry_run: bool, add: bool) -> Result<Vec<Outcome>> {
    let bin = binary_path()?;
    let mut out = Vec::new();

    for client in known_clients() {
        if !only.is_empty() && !only.iter().any(|o| o == client.id) {
            continue;
        }
        if !client.installed() {
            continue;
        }
        let changed = match client.format {
            Format::Json => edit_json(client.path(), &bin, add, dry_run)?,
            Format::Toml => edit_toml(client.path(), &bin, add, dry_run)?,
        };
        out.push(Outcome {
            label: client.label,
            path: client.path().to_path_buf(),
            changed,
            needs_restart: client.needs_restart && changed,
        });
    }
    Ok(out)
}

/// Copy the file aside once, the first time we modify it.
fn backup_once(path: &Path) -> Result<()> {
    let bak = path.with_extension(format!(
        "{}.whisper-push.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    if !bak.exists() {
        std::fs::copy(path, &bak)
            .with_context(|| format!("Failed to back up {}", path.display()))?;
    }
    Ok(())
}

/// Temp file in the same directory, then rename — so a crash mid-write leaves
/// the original intact rather than a truncated config.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("whisper-push.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path).with_context(|| format!("Failed to replace {}", path.display()))?;
    Ok(())
}

/// Our entry, in the shape every JSON client expects.
fn json_entry(bin: &str) -> serde_json::Value {
    serde_json::json!({ "command": bin, "args": ["mcp"] })
}

fn edit_json(path: &Path, bin: &str, add: bool, dry_run: bool) -> Result<bool> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut root: serde_json::Value = if raw.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&raw)
            .with_context(|| format!("{} is not valid JSON", path.display()))?
    };
    if !root.is_object() {
        bail!("{} is not a JSON object", path.display());
    }

    let servers = root
        .as_object_mut()
        .expect("checked")
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        bail!("{} has a non-object \"mcpServers\"", path.display());
    }
    let servers = servers.as_object_mut().expect("checked");

    let desired = json_entry(bin);
    let changed = if add {
        if servers.get(SERVER_NAME) == Some(&desired) {
            false // already correct — do not rewrite the file for nothing
        } else {
            servers.insert(SERVER_NAME.into(), desired);
            true
        }
    } else {
        servers.remove(SERVER_NAME).is_some()
    };

    if changed && !dry_run {
        backup_once(path)?;
        write_atomic(path, &format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    }
    Ok(changed)
}

fn edit_toml(path: &Path, bin: &str, add: bool, dry_run: bool) -> Result<bool> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))?;

    let changed = if add {
        let mut table = toml_edit::Table::new();
        table["command"] = toml_edit::value(bin);
        let mut args = toml_edit::Array::new();
        args.push("mcp");
        table["args"] = toml_edit::value(args);

        let existing = doc
            .get("mcp_servers")
            .and_then(|s| s.get(SERVER_NAME))
            .map(|t| t.to_string());
        let desired = toml_edit::Item::Table(table.clone()).to_string();
        if existing.as_deref() == Some(desired.as_str()) {
            false
        } else {
            doc["mcp_servers"][SERVER_NAME] = toml_edit::Item::Table(table);
            true
        }
    } else {
        doc.get_mut("mcp_servers")
            .and_then(|s| s.as_table_like_mut())
            .map(|t| t.remove(SERVER_NAME).is_some())
            .unwrap_or(false)
    };

    if changed && !dry_run {
        backup_once(path)?;
        write_atomic(path, &doc.to_string())?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str, contents: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("wp-mcp-test-{name}"));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        p
    }

    #[test]
    fn json_add_preserves_other_servers() {
        let p = tmp(
            "other.json",
            r#"{"mcpServers":{"slack":{"command":"slack-mcp"}},"theme":"dark"}"#,
        );
        assert!(edit_json(&p, "/bin/wp", true, false).unwrap());

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["slack"]["command"], "slack-mcp");
        assert_eq!(v["mcpServers"]["whisper-push"]["command"], "/bin/wp");
        // Unrelated top-level keys must survive.
        assert_eq!(v["theme"], "dark");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn json_add_is_idempotent() {
        let p = tmp("idem.json", r#"{"mcpServers":{}}"#);
        assert!(edit_json(&p, "/bin/wp", true, false).unwrap());
        // Second run reports "nothing changed" rather than rewriting.
        assert!(!edit_json(&p, "/bin/wp", true, false).unwrap());
        // A moved binary IS a change.
        assert!(edit_json(&p, "/opt/wp", true, false).unwrap());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn json_uninstall_removes_only_us() {
        let p = tmp(
            "rm.json",
            r#"{"mcpServers":{"slack":{"command":"s"},"whisper-push":{"command":"x"}}}"#,
        );
        assert!(edit_json(&p, "/bin/wp", false, false).unwrap());
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert!(v["mcpServers"]["whisper-push"].is_null());
        assert_eq!(v["mcpServers"]["slack"]["command"], "s");
        // Removing twice is not an error, just "no change".
        assert!(!edit_json(&p, "/bin/wp", false, false).unwrap());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn json_dry_run_writes_nothing() {
        let before = r#"{"mcpServers":{}}"#;
        let p = tmp("dry.json", before);
        assert!(edit_json(&p, "/bin/wp", true, true).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn toml_add_preserves_comments_and_neighbours() {
        let p = tmp(
            "cfg.toml",
            "# my hand-written config\nmodel = \"gpt-5\"\n\n[mcp_servers.slack]\ncommand = \"slack-mcp\"\n",
        );
        assert!(edit_toml(&p, "/bin/wp", true, false).unwrap());

        let s = std::fs::read_to_string(&p).unwrap();
        // The comment is the whole reason we use toml_edit.
        assert!(s.contains("# my hand-written config"), "{s}");
        assert!(s.contains("model = \"gpt-5\""), "{s}");
        assert!(s.contains("[mcp_servers.slack]"), "{s}");
        assert!(s.contains("[mcp_servers.whisper-push]"), "{s}");
        assert!(s.contains("/bin/wp"), "{s}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn toml_add_is_idempotent_and_removable() {
        let p = tmp("idem.toml", "[mcp_servers.slack]\ncommand = \"s\"\n");
        assert!(edit_toml(&p, "/bin/wp", true, false).unwrap());
        assert!(!edit_toml(&p, "/bin/wp", true, false).unwrap());
        assert!(edit_toml(&p, "/bin/wp", false, false).unwrap());

        let s = std::fs::read_to_string(&p).unwrap();
        assert!(!s.contains("whisper-push"), "{s}");
        assert!(s.contains("[mcp_servers.slack]"), "{s}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn corrupt_config_is_reported_not_overwritten() {
        let broken = "{ this is not json";
        let p = tmp("broken.json", broken);
        assert!(edit_json(&p, "/bin/wp", true, false).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), broken);
        let _ = std::fs::remove_file(&p);
    }
}
