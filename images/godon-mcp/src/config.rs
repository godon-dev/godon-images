pub struct Config {
    pub port: u16,
    pub api_hostname: String,
    pub api_port: u16,
    pub api_insecure: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            port: std::env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(3001),
            api_hostname: std::env::var("GODON_API_HOSTNAME")
                .unwrap_or_else(|_| "localhost".to_string()),
            api_port: std::env::var("GODON_API_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080),
            api_insecure: std::env::var("GODON_API_INSECURE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
        }
    }
}
