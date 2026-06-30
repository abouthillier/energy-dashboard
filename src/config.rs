use std::{env, path::PathBuf};

use anyhow::Context;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub invoice_dir: PathBuf,
    pub host: String,
    pub port: u16,
    pub weather_lat: Option<f64>,
    pub weather_lon: Option<f64>,
    pub weather_timezone: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = env::var("ENERGY_DATABASE_URL")
            .unwrap_or_else(|_| "sqlite:/data/energy.sqlite".to_string());
        let invoice_dir = env::var("ENERGY_INVOICE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/invoices"));
        let host = env::var("ENERGY_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = env::var("ENERGY_PORT")
            .unwrap_or_else(|_| "3010".to_string())
            .parse()
            .context("ENERGY_PORT must be a number")?;
        let weather_lat = env::var("ENERGY_WEATHER_LAT").ok().and_then(|value| value.parse().ok());
        let weather_lon = env::var("ENERGY_WEATHER_LON").ok().and_then(|value| value.parse().ok());
        let weather_timezone = env::var("ENERGY_WEATHER_TIMEZONE")
            .unwrap_or_else(|_| "America/New_York".to_string());

        Ok(Self {
            database_url,
            invoice_dir,
            host,
            port,
            weather_lat,
            weather_lon,
            weather_timezone,
        })
    }
}
