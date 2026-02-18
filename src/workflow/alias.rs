use crate::config::types::AgentConfig;
use crate::error::{OrchestratorError, Result};

/// Resolve an alias to its agent configuration.
pub fn resolve_alias<'a>(alias: &str, agents: &'a [AgentConfig]) -> Result<&'a AgentConfig> {
    agents
        .iter()
        .find(|a| a.alias.eq_ignore_ascii_case(alias))
        .ok_or_else(|| OrchestratorError::UnknownAlias(alias.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::AgentRole;

    fn test_agents() -> Vec<AgentConfig> {
        vec![
            AgentConfig {
                alias: "operator".into(),
                identity: "Claude".into(),
                backend: "stub".into(),
                role: AgentRole::Operator,

                model: None,
                models: None,
                preferred_models: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: None,
                backend_args: None,
                env: None,
            },
            AgentConfig {
                alias: "focused".into(),
                identity: "Claude".into(),
                backend: "stub".into(),
                role: AgentRole::Worker,

                model: None,
                models: None,
                preferred_models: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: None,
                backend_args: None,
                env: None,
            },
        ]
    }

    #[test]
    fn test_resolve_alias_found() {
        let agents = test_agents();
        let result = resolve_alias("operator", &agents);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().alias, "operator");
    }

    #[test]
    fn test_resolve_alias_case_insensitive() {
        let agents = test_agents();
        let result = resolve_alias("OPERATOR", &agents);
        assert!(result.is_ok());
    }

    #[test]
    fn test_resolve_alias_not_found() {
        let agents = test_agents();
        let result = resolve_alias("unknown", &agents);
        assert!(result.is_err());
    }
}
