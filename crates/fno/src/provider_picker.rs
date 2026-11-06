//! The Accounts tab's "add a provider" picker: the Popular list, a search
//! over the models.dev catalog, and the Local servers that take a base URL
//! instead of a key. Pure data and filtering; the wizard in
//! `connections_view` drives it.

use crate::model_catalog::{Catalog, CatalogProvider};

/// Providers shown first, as `(catalog id, title)`. The Accounts tab's
/// always-listed cards come from the same seven.
pub const POPULAR: [(&str, &str); 8] = [
    ("anthropic", "Anthropic"),
    ("openai", "OpenAI"),
    ("openrouter", "OpenRouter"),
    ("zai", "zai"),
    ("bedrock", "Amazon Bedrock"),
    ("deepseek", "DeepSeek"),
    ("opencode", "OpenCode"),
    ("llama", "Llama"),
];

/// A local model server: it takes a base URL, not a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalServer {
    pub key: &'static str,
    pub title: &'static str,
    /// The origin the server listens on by default.
    pub origin: &'static str,
    /// The path that lists the models the server holds.
    pub models_path: &'static str,
}

pub const LOCAL: [LocalServer; 5] = [
    LocalServer {
        key: "ollama",
        title: "Ollama",
        origin: "http://127.0.0.1:11434",
        models_path: "/api/tags",
    },
    LocalServer {
        key: "lmstudio",
        title: "LM Studio",
        origin: "http://127.0.0.1:1234",
        models_path: "/v1/models",
    },
    LocalServer {
        key: "vllm",
        title: "vLLM",
        origin: "http://127.0.0.1:8000",
        models_path: "/v1/models",
    },
    LocalServer {
        key: "llamacpp",
        title: "llama.cpp",
        origin: "http://127.0.0.1:8080",
        models_path: "/v1/models",
    },
    LocalServer {
        key: "mlx",
        title: "MLX",
        origin: "http://127.0.0.1:8081",
        models_path: "/v1/models",
    },
];

/// One picker row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A catalog provider that takes a key. `popular` rows sort first.
    Catalog {
        id: String,
        title: String,
        popular: bool,
    },
    Local(LocalServer),
}

impl Entry {
    pub fn title(&self) -> &str {
        match self {
            Entry::Catalog { title, .. } => title,
            Entry::Local(s) => s.title,
        }
    }
}

/// The rows for `query`: Popular in its order, then Local, then the rest of
/// the catalog A to Z. A row matches when the query is a case-insensitive
/// substring of its title or id. Catalog providers without an `api` and an
/// `npm` entry cannot run anywhere, so they stay out.
pub fn entries(catalog: Option<&Catalog>, query: &str) -> Vec<Entry> {
    let q = query.trim().to_ascii_lowercase();
    let hit = |id: &str, title: &str| {
        q.is_empty()
            || id.to_ascii_lowercase().contains(&q)
            || title.to_ascii_lowercase().contains(&q)
    };
    let mut out: Vec<Entry> = POPULAR
        .iter()
        .filter(|(id, title)| hit(id, title))
        .map(|(id, title)| Entry::Catalog {
            id: id.to_string(),
            title: title.to_string(),
            popular: true,
        })
        .collect();
    out.extend(
        LOCAL
            .iter()
            .filter(|s| hit(s.key, s.title))
            .map(|s| Entry::Local(*s)),
    );
    if let Some(cat) = catalog {
        let mut rest: Vec<(&String, &CatalogProvider)> = cat
            .providers
            .iter()
            .filter(|(id, cp)| {
                cp.api.is_some()
                    && cp.npm.is_some()
                    && !POPULAR.iter().any(|(p, _)| p == &id.as_str())
                    && hit(id, display_name(id, cp))
            })
            .collect();
        rest.sort_by_key(|(id, cp)| display_name(id, cp).to_ascii_lowercase());
        out.extend(rest.into_iter().map(|(id, cp)| Entry::Catalog {
            id: id.clone(),
            title: display_name(id, cp).to_string(),
            popular: false,
        }));
    }
    out
}

fn display_name<'a>(id: &'a str, cp: &'a CatalogProvider) -> &'a str {
    if cp.name.is_empty() {
        id
    } else {
        &cp.name
    }
}

/// Model ids from a local server's models reply: ollama's `models[].name`,
/// or the openai-style `data[].id`.
pub fn parse_local_models(body: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let pick = |arr: Option<&Vec<serde_json::Value>>, field: &str| -> Vec<String> {
        arr.map(|a| {
            a.iter()
                .filter_map(|m| m.get(field).and_then(|s| s.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
    };
    let mut ids = pick(v.get("models").and_then(|m| m.as_array()), "name");
    if ids.is_empty() {
        ids = pick(v.get("data").and_then(|m| m.as_array()), "id");
    }
    ids
}

/// The openai-compatible base URL for a server origin the user typed.
pub fn openai_base(origin: &str) -> String {
    let o = origin.trim().trim_end_matches('/');
    if o.ends_with("/v1") {
        o.to_string()
    } else {
        format!("{o}/v1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        let json = r#"{
            "xai": {"name": "xAI", "env": ["XAI_API_KEY"], "api": "https://api.x.ai/v1", "npm": "@ai-sdk/openai-compatible", "models": {"grok-4": {}}},
            "openrouter": {"name": "OpenRouter", "env": ["OPENROUTER_API_KEY"], "api": "https://openrouter.ai/api/v1", "npm": "@ai-sdk/openai-compatible", "models": {}},
            "bare": {"name": "Bare", "models": {}}
        }"#;
        crate::model_catalog::parse(json).unwrap()
    }

    #[test]
    fn popular_leads_then_local_then_catalog_az_and_unrunnable_stay_out() {
        let titles: Vec<String> = entries(Some(&catalog()), "")
            .iter()
            .map(|e| e.title().to_string())
            .collect();
        assert_eq!(titles[2], "OpenRouter");
        assert_eq!(titles[8], "Ollama");
        assert_eq!(titles.last().map(String::as_str), Some("xAI"));
        assert!(!titles.iter().any(|t| t == "Bare"));
    }
}
