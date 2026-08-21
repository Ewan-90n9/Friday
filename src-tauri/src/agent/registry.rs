pub struct AgentDescriptor {
    pub provider: &'static str,
    pub command: &'static str,
    pub display_name: &'static str,
}

pub const REGISTRY: &[AgentDescriptor] = &[
    AgentDescriptor {
        provider: "opencode",
        command: "opencode",
        display_name: "OpenCode",
    },
    AgentDescriptor {
        provider: "codeagentcli",
        command: "codeagentcli",
        display_name: "CodeAgentCLI",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_opencode_and_codeagentcli() {
        assert_eq!(REGISTRY.len(), 2);

        let opencode = REGISTRY.iter().find(|d| d.provider == "opencode").unwrap();
        assert_eq!(opencode.command, "opencode");
        assert_eq!(opencode.display_name, "OpenCode");

        let codeagent = REGISTRY.iter().find(|d| d.provider == "codeagentcli").unwrap();
        assert_eq!(codeagent.command, "codeagentcli");
        assert_eq!(codeagent.display_name, "CodeAgentCLI");
    }
}
