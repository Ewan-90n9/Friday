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
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_exactly_one_opencode_entry() {
        assert_eq!(REGISTRY.len(), 1);
        let entry = &REGISTRY[0];
        assert_eq!(entry.provider, "opencode");
        assert_eq!(entry.command, "opencode");
        assert_eq!(entry.display_name, "OpenCode");
    }
}
