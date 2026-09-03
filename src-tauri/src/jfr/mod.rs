pub mod client;
pub mod manager;

pub use manager::{
    download_complete_hook, production_client_factory, JmcConfig, JmcError, JmcManager,
    JMC_JAR_NAME,
};
