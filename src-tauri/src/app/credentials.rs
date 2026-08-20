pub async fn store_secret(
    _env_id: &str,
    _key: &str,
    _value: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    todo!()
}

pub async fn load_secret(
    _env_id: &str,
    _key: &str,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    todo!()
}
