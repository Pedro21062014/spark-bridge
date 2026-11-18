use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityPolicy {
    #[serde(default = "default_profile")]
    pub profile: String,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub blocked_requests: Vec<String>,
    #[serde(default)]
    pub blocked_commands: Vec<String>,
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    #[serde(default)]
    pub blocked_apps: Vec<String>,
    #[serde(default)]
    pub allowed_apps: Vec<String>,
    #[serde(default = "default_true")]
    pub block_unknown_domains: bool,
    #[serde(default = "default_true")]
    pub block_unknown_commands: bool,
    #[serde(default = "default_true")]
    pub block_unknown_apps: bool,
}

fn default_profile() -> String { "custom".to_string() }
fn default_true() -> bool { true }

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            profile: "safe".to_string(),
            blocked_domains: Vec::new(),
            allowed_domains: vec!["localhost".to_string(), "127.0.0.1".to_string()],
            blocked_requests: Vec::new(),
            blocked_commands: vec![
                "format ".into(), " del ".into(), " rm -rf".into(), "shutdown".into(),
                "reboot".into(), "poweroff".into(), "reg delete".into(), "mkfs".into(),
                "curl | sh".into(), "wget | sh".into(),
            ],
            allowed_commands: Vec::new(),
            blocked_apps: Vec::new(),
            allowed_apps: Vec::new(),
            block_unknown_domains: false,
            block_unknown_commands: false,
            block_unknown_apps: false,
        }
    }
}

fn normalized_list(items: &[String]) -> Vec<String> {
    items.iter().map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect()
}

fn matches_pattern(value: &str, patterns: &[String]) -> bool {
    let value = value.to_ascii_lowercase();
    normalized_list(patterns).iter().any(|pattern| value.contains(pattern))
}

fn matches_domain(host: &str, patterns: &[String]) -> bool {
    normalized_list(patterns).iter().any(|pattern| host == pattern || host.ends_with(&format!(".{}", pattern)))
}

pub fn tool_block_reason(policy: &SecurityPolicy, tool: &str) -> Option<String> {
    if matches_pattern(tool, &policy.blocked_requests) {
        return Some(format!("requisição/ferramenta '{}' bloqueada pela política", tool));
    }
    None
}

pub fn command_block_reason(policy: &SecurityPolicy, command: &str) -> Option<String> {
    let command = command.trim();
    if matches_pattern(command, &policy.blocked_commands) {
        return Some("comando bloqueado pela lista de comandos perigosos/personalizados".into());
    }
    if policy.block_unknown_commands && !policy.allowed_commands.is_empty() && !matches_pattern(command, &policy.allowed_commands) {
        return Some("comando não está na lista de comandos permitidos".into());
    }
    if policy.block_unknown_commands && policy.allowed_commands.is_empty() {
        return Some("execução de comandos não permitida pelo perfil atual".into());
    }
    let executable = command.split_whitespace().next().unwrap_or(command);
    if matches_pattern(executable, &policy.blocked_apps) {
        return Some("aplicativo usado pelo comando está bloqueado".into());
    }
    if policy.block_unknown_apps && !policy.allowed_apps.is_empty() && !matches_pattern(executable, &policy.allowed_apps) {
        return Some("aplicativo não está na lista de aplicativos permitidos".into());
    }
    if policy.block_unknown_apps && policy.allowed_apps.is_empty() {
        return Some("execução de aplicativos não permitida pelo perfil atual".into());
    }
    None
}

pub fn url_block_reason(policy: &SecurityPolicy, url: &str) -> Option<String> {
    let parsed = match url::Url::parse(url) {
        Ok(value) => value,
        Err(_) => return Some("URL inválida".into()),
    };
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if matches_domain(&host, &policy.blocked_domains) {
        return Some(format!("domínio '{}' bloqueado pela política", host));
    }
    if policy.block_unknown_domains && !policy.allowed_domains.is_empty() && !matches_domain(&host, &policy.allowed_domains) {
        return Some(format!("domínio '{}' não está na lista permitida", host));
    }
    if policy.block_unknown_domains && policy.allowed_domains.is_empty() {
        return Some("acesso a domínios externos não permitido pelo perfil atual".into());
    }
    None
}
