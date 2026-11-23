//! 按 Agent 读取本机的可选模型列表。
//!
//! 各 CLI 的模型来源不同，用户又常经由网关（如 magpie）接入自定义模型，
//! 所以不写死清单，而是从各自的本机配置里读：
//! - claude-cli：`~/.claude/settings.json` → `modelPicker.options`
//! - codex-cli ：`~/.codex/config.toml` → `model_catalog_json` 指向的目录（`models[].slug`）
//! - grok-cli   ：`~/.grok/config.toml` → `[model.<id>]`
//! - pi         ：`pi --list-models`
//!
//! 配置文件里带有密钥，这里只在内存中解析出模型名，不做任何记录或返回。
//! 清单之外的模型可以手动填写，由 [`model_remember`] 记住。

use crate::adapters;
use crate::discovery;
use crate::AppState;
use rusqlite::params;
use serde::Serialize;
use serde_json::Value as Json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tauri::State;
use tokio::process::Command;

/// `pi --list-models` 的超时：它可能联网刷新模型目录
const LIST_MODELS_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    /// 传给 CLI 的模型标识
    pub value: String,
    pub label: String,
    /// 补充说明（来源渠道等）
    pub detail: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelList {
    pub adapter: String,
    /// 模型清单的来源，便于排查「为什么没有我的模型」
    pub source: String,
    /// 不选模型时 CLI 实际使用的默认值
    pub default: Option<String>,
    pub options: Vec<ModelOption>,
    /// 用户手动填写过的模型
    pub custom: Vec<String>,
}

/// (默认模型, 可选模型, 清单来源)
type Loaded = (Option<String>, Vec<ModelOption>, String);

fn opt(value: &str, label: Option<&str>, detail: Option<&str>) -> ModelOption {
    ModelOption {
        value: value.to_string(),
        label: label.filter(|l| !l.is_empty()).unwrap_or(value).to_string(),
        detail: detail.map(str::to_string),
    }
}

fn dedup(list: Vec<ModelOption>) -> Vec<ModelOption> {
    let mut seen = std::collections::HashSet::new();
    list.into_iter()
        .filter(|m| seen.insert(m.value.clone()))
        .collect()
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// claude-cli。只有官方直连（没有自定义 modelPicker）时才回退到内置别名。
fn claude_models(home: &Path) -> Loaded {
    let rel = "~/.claude/settings.json";
    let parsed = read(&home.join(".claude/settings.json"))
        .and_then(|text| serde_json::from_str::<Json>(&text).ok());
    let Some(v) = parsed else {
        return (None, builtin_claude(), format!("{rel}（未找到，使用内置清单）"));
    };
    let default = v["model"].as_str().map(str::to_string);
    let options: Vec<ModelOption> = v["modelPicker"]["options"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|o| {
                    Some(opt(
                        o["model"].as_str()?,
                        o["label"].as_str(),
                        o["description"].as_str(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    if options.is_empty() {
        (
            default,
            builtin_claude(),
            format!("{rel}（未配置 modelPicker，使用内置清单）"),
        )
    } else {
        (default, options, rel.to_string())
    }
}

/// 官方直连时的模型别名
fn builtin_claude() -> Vec<ModelOption> {
    [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")]
        .into_iter()
        .map(|(value, label)| opt(value, Some(label), None))
        .collect()
}

/// codex-cli。模型目录由 config.toml 的 `model_catalog_json` 指定。
fn codex_models(home: &Path) -> Loaded {
    let rel = "~/.codex/config.toml";
    let parsed = read(&home.join(".codex/config.toml"))
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok());
    let Some(cfg) = parsed else {
        return (None, Vec::new(), format!("{rel}（未找到）"));
    };
    let default = cfg.get("model").and_then(|m| m.as_str()).map(str::to_string);
    let Some(catalog) = cfg.get("model_catalog_json").and_then(|p| p.as_str()) else {
        return (
            default,
            Vec::new(),
            format!("{rel}（未配置 model_catalog_json）"),
        );
    };
    // 相对路径按 codex 的惯例相对于 `~/.codex` 解析
    let catalog = if Path::new(catalog).is_absolute() {
        PathBuf::from(catalog)
    } else {
        home.join(".codex").join(catalog)
    };
    let parsed = read(&catalog).and_then(|text| serde_json::from_str::<Json>(&text).ok());
    let Some(v) = parsed else {
        return (
            default,
            Vec::new(),
            format!("{}（未找到或无法解析）", catalog.display()),
        );
    };
    let options: Vec<ModelOption> = v["models"]
        .as_array()
        .map(|list| {
            list.iter()
                // hide 的是内部模型（自动审查等），不出现在选择器里
                .filter(|m| m["visibility"].as_str() != Some("hide"))
                .filter_map(|m| {
                    Some(opt(
                        m["slug"].as_str()?,
                        m["display_name"].as_str(),
                        m["description"].as_str(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    (default, options, catalog.display().to_string())
}

/// grok-cli。`-m` 接受的是 `[model.<id>]` 的表名，展示名取其 `name`。
fn grok_models(home: &Path) -> Loaded {
    let rel = "~/.grok/config.toml";
    let parsed = read(&home.join(".grok/config.toml"))
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok());
    let Some(cfg) = parsed else {
        return (None, Vec::new(), format!("{rel}（未找到）"));
    };
    let default = cfg
        .get("models")
        .and_then(|m| m.get("default"))
        .and_then(|d| d.as_str())
        .map(str::to_string);
    let options = cfg
        .get("model")
        .and_then(|m| m.as_table())
        .map(|table| {
            table
                .iter()
                .map(|(id, m)| opt(id, m.get("name").and_then(|n| n.as_str()), None))
                .collect()
        })
        .unwrap_or_default();
    (default, options, rel.to_string())
}

/// pi。模型标识是 `provider/model`，清单来自 `pi --list-models`。
async fn pi_models(home: &Path, bin: Option<PathBuf>) -> Loaded {
    let mut default = None;
    if let Some(v) = read(&home.join(".pi/agent/settings.json"))
        .and_then(|text| serde_json::from_str::<Json>(&text).ok())
    {
        if let (Some(provider), Some(model)) = (
            v["defaultProvider"].as_str(),
            v["defaultModel"].as_str(),
        ) {
            default = Some(format!("{provider}/{model}"));
        }
    }
    let rel = "pi --list-models";
    let Some(bin) = bin else {
        return (default, Vec::new(), format!("{rel}（未找到 pi）"));
    };
    let output = tokio::time::timeout(
        LIST_MODELS_TIMEOUT,
        Command::new(bin)
            .arg("--list-models")
            .env("PATH", discovery::child_path())
            .stdin(Stdio::null())
            .output(),
    )
    .await;
    let Ok(Ok(output)) = output else {
        return (
            default,
            Vec::new(),
            format!("{rel}（执行超时或失败）"),
        );
    };
    // 表格列：provider  model  context  max-out  thinking  images
    let options = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let provider = cols.next()?;
            let model = cols.next()?;
            (provider != "provider").then(|| {
                opt(
                    &format!("{provider}/{model}"),
                    Some(model),
                    Some(provider),
                )
            })
        })
        .collect();
    (default, options, rel.to_string())
}

/// 某个 Agent 可选的模型：本机配置里读到的 + 用户手动填写过的。
#[tauri::command]
pub async fn agent_models(
    state: State<'_, Arc<AppState>>,
    adapter: String,
) -> Result<ModelList, String> {
    let home = discovery::home_dir();
    let (default, options, source) = match adapter.as_str() {
        "claude-cli" => claude_models(&home),
        "codex-cli" => codex_models(&home),
        "grok-cli" => grok_models(&home),
        "pi" => pi_models(&home, adapters::resolve_binary("pi")).await,
        _ => return Err(format!("「{adapter}」暂不支持会话")),
    };
    let custom = {
        let db = state.db.lock().unwrap();
        let mut stmt = db
            .prepare(
                "SELECT model FROM custom_models WHERE adapter = ?1 ORDER BY created_at DESC, model",
            )
            .map_err(|e| e.to_string())?;
        stmt.query_map(params![adapter], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| e.to_string())?
    };
    Ok(ModelList {
        adapter,
        source,
        default,
        options: dedup(options),
        custom,
    })
}

/// 记住用户手动填写的模型，下次出现在该 Agent 的下拉里。
#[tauri::command]
pub fn model_remember(
    state: State<'_, Arc<AppState>>,
    adapter: String,
    model: String,
) -> Result<(), String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("模型 ID 不能为空".into());
    }
    let db = state.db.lock().unwrap();
    db.execute(
        "INSERT OR IGNORE INTO custom_models (adapter, model) VALUES (?1, ?2)",
        params![adapter, model],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt_strs(list: &[ModelOption]) -> Vec<(&str, &str)> {
        list.iter().map(|m| (m.value.as_str(), m.label.as_str())).collect()
    }

    #[test]
    fn claude_reads_model_picker() {
        let dir = std::env::temp_dir().join("aa-models-test/claude");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(
            dir.join(".claude/settings.json"),
            r#"{
                "model": "sonnet[1m]",
                "env": {"ANTHROPIC_AUTH_TOKEN": "secret"},
                "modelPicker": {"options": [
                    {"label": "GLM-5.3 · routing group", "model": "auto/glm-5.3[1m]", "description": "via magpie"},
                    {"label": "dup", "model": "auto/glm-5.3[1m]"}
                ]}
            }"#,
        )
        .unwrap();
        let (default, options, source) = claude_models(&dir);
        assert_eq!(default.as_deref(), Some("sonnet[1m]"));
        assert_eq!(
            opt_strs(&dedup(options)),
            [("auto/glm-5.3[1m]", "GLM-5.3 · routing group")]
        );
        assert!(source.contains("settings.json"));
    }

    #[test]
    fn claude_falls_back_to_builtin_aliases() {
        let dir = std::env::temp_dir().join("aa-models-test/claude-empty");
        std::fs::create_dir_all(&dir).unwrap();
        let (_, options, source) = claude_models(&dir);
        assert_eq!(
            opt_strs(&options),
            [("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")]
        );
        assert!(source.contains("内置"));
    }

    #[test]
    fn grok_reads_model_table_in_file_order() {
        let dir = std::env::temp_dir().join("aa-models-test/grok");
        std::fs::create_dir_all(dir.join(".grok")).unwrap();
        std::fs::write(
            dir.join(".grok/config.toml"),
            r#"
[ui]
yolo = false
[models]
default = "magpie/zai/glm-5.3"
[model]
"magpie/zai/glm-5.3" = { model = "zai/glm-5.3", name = "GLM-5.3 · Z.AI", api_key = "secret" }
"magpie/auto/glm-5.3-flash" = { model = "auto/glm-5.3-flash", name = "GLM-5.3-Flash · routing group", api_key = "secret" }
"#,
        )
        .unwrap();
        let (default, options, _) = grok_models(&dir);
        assert_eq!(default.as_deref(), Some("magpie/zai/glm-5.3"));
        assert_eq!(
            opt_strs(&options),
            [
                ("magpie/zai/glm-5.3", "GLM-5.3 · Z.AI"),
                ("magpie/auto/glm-5.3-flash", "GLM-5.3-Flash · routing group")
            ]
        );
    }

    #[test]
    fn codex_reads_catalog_and_skips_hidden() {
        let dir = std::env::temp_dir().join("aa-models-test/codex");
        std::fs::create_dir_all(dir.join(".codex")).unwrap();
        std::fs::write(
            dir.join(".codex/config.toml"),
            "model = \"workbuddy/deepseek-v4.1-flash\"\nmodel_catalog_json = \"catalog.json\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".codex/catalog.json"),
            r#"{"models": [
                {"slug": "auto/glm-5.3-flash", "display_name": "GLM-5.3-Flash", "visibility": "list"},
                {"slug": "codex-auto-review", "display_name": "Auto Review", "visibility": "hide"}
            ]}"#,
        )
        .unwrap();
        let (default, options, source) = codex_models(&dir);
        assert_eq!(default.as_deref(), Some("workbuddy/deepseek-v4.1-flash"));
        assert_eq!(opt_strs(&options), [("auto/glm-5.3-flash", "GLM-5.3-Flash")]);
        assert!(source.ends_with("catalog.json"), "{source}");
    }
}
