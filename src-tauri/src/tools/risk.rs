use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    ReadOnly,
    Low,
    High,
}

/// Low/High 风险工具且未开启免确认模式时才需要用户确认
pub fn should_confirm(risk_level: RiskLevel, auto_approve: bool) -> bool {
    matches!(risk_level, RiskLevel::Low | RiskLevel::High) && !auto_approve
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_confirm_truth_table() {
        // 关闭（默认）：ReadOnly 直通，Low/High 需确认
        assert!(!should_confirm(RiskLevel::ReadOnly, false));
        assert!(should_confirm(RiskLevel::Low, false));
        assert!(should_confirm(RiskLevel::High, false));
        // 开启：全部免确认
        assert!(!should_confirm(RiskLevel::ReadOnly, true));
        assert!(!should_confirm(RiskLevel::Low, true));
        assert!(!should_confirm(RiskLevel::High, true));
    }
}
