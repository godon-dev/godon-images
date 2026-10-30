pub struct Config {
    pub port: u16,
    pub api_hostname: String,
    pub api_port: u16,
    pub api_insecure: bool,
    pub causal_hostname: String,
    pub causal_port: u16,
    pub causal_insecure: bool,
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
            causal_hostname: std::env::var("GODON_CAUSAL_HOSTNAME")
                .unwrap_or_else(|_| "localhost".to_string()),
            causal_port: std::env::var("GODON_CAUSAL_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(9091),
            causal_insecure: std::env::var("GODON_CAUSAL_INSECURE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn clear() {
        for k in [
            "PORT",
            "GODON_API_HOSTNAME",
            "GODON_API_PORT",
            "GODON_API_INSECURE",
            "GODON_CAUSAL_HOSTNAME",
            "GODON_CAUSAL_PORT",
            "GODON_CAUSAL_INSECURE",
        ] {
            env::remove_var(k);
        }
    }

    // one serial test: the scenarios share process env, and cargo runs
    // test fns in parallel — a race here once (the estate's known pitfall)
    #[test]
    fn defaults_overrides_and_insecure_parsing_in_one_serial_pass() {
        let saved = clear_and_save();
        clear();

        // defaults when nothing is set
        let c = Config::from_env();
        assert_eq!(c.port, 3001);
        assert_eq!(c.api_hostname, "localhost");
        assert_eq!(c.api_port, 8080);
        assert!(!c.api_insecure);
        assert_eq!(c.causal_hostname, "localhost");
        assert_eq!(c.causal_port, 9091);
        assert!(!c.causal_insecure);

        // ports parse; unparseable falls back
        env::set_var("PORT", "4242");
        env::set_var("GODON_CAUSAL_PORT", "not-a-number");
        let c = Config::from_env();
        assert_eq!(c.port, 4242);
        assert_eq!(c.causal_port, 9091);
        env::remove_var("PORT");
        env::remove_var("GODON_CAUSAL_PORT");

        // insecure accepts true and 1, not yes
        env::set_var("GODON_API_INSECURE", "true");
        env::set_var("GODON_CAUSAL_INSECURE", "1");
        assert!(Config::from_env().api_insecure);
        assert!(Config::from_env().causal_insecure);
        env::set_var("GODON_API_INSECURE", "yes");
        assert!(!Config::from_env().api_insecure);

        restore(saved);
    }

    fn clear_and_save() -> Vec<(&'static str, Option<String>)> {
        let keys = [
            "PORT",
            "GODON_API_HOSTNAME",
            "GODON_API_PORT",
            "GODON_API_INSECURE",
            "GODON_CAUSAL_HOSTNAME",
            "GODON_CAUSAL_PORT",
            "GODON_CAUSAL_INSECURE",
        ];
        keys.iter()
            .map(|k| {
                let v = env::var(k).ok();
                env::remove_var(k);
                (*k, v)
            })
            .collect()
    }

    fn restore(saved: Vec<(&'static str, Option<String>)>) {
        for (k, v) in saved {
            if let Some(v) = v {
                env::set_var(k, v);
            }
        }
    }
}
