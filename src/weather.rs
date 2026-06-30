use anyhow::Context;
use chrono::{Datelike, Duration, NaiveDate};
use serde::Deserialize;
use sqlx::{Row, SqlitePool};

use crate::config::Config;

#[derive(Debug, Deserialize)]
struct OpenMeteoResponse {
    daily: OpenMeteoDaily,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoDaily {
    temperature_2m_mean: Vec<f64>,
}

pub async fn enrich_missing_weather(pool: &SqlitePool, config: &Config) -> anyhow::Result<usize> {
    let (Some(lat), Some(lon)) = (config.weather_lat, config.weather_lon) else {
        return Ok(0);
    };

    let rows = sqlx::query(
        r#"
        SELECT DISTINCT bill_month
        FROM utility_bills
        WHERE bill_month != 'unknown'
          AND bill_month NOT IN (SELECT month FROM weather_monthly)
        ORDER BY bill_month
        "#,
    )
    .fetch_all(pool)
    .await?;

    let client = reqwest::Client::new();
    let mut enriched = 0;

    for row in rows {
        let month: String = row.get("bill_month");
        if let Some(metrics) = fetch_month(&client, lat, lon, &config.weather_timezone, &month).await? {
            sqlx::query(
                r#"
                INSERT INTO weather_monthly (
                  month, avg_temp_f, heating_degree_days, cooling_degree_days,
                  source, latitude, longitude
                )
                VALUES (?, ?, ?, ?, 'open-meteo', ?, ?)
                ON CONFLICT(month) DO UPDATE SET
                  avg_temp_f = excluded.avg_temp_f,
                  heating_degree_days = excluded.heating_degree_days,
                  cooling_degree_days = excluded.cooling_degree_days,
                  source = excluded.source,
                  latitude = excluded.latitude,
                  longitude = excluded.longitude,
                  fetched_at = datetime('now')
                "#,
            )
            .bind(&month)
            .bind(metrics.avg_temp_f)
            .bind(metrics.heating_degree_days)
            .bind(metrics.cooling_degree_days)
            .bind(lat)
            .bind(lon)
            .execute(pool)
            .await?;
            enriched += 1;
        }
    }

    Ok(enriched)
}

#[derive(Debug)]
struct WeatherMetrics {
    avg_temp_f: f64,
    heating_degree_days: f64,
    cooling_degree_days: f64,
}

async fn fetch_month(
    client: &reqwest::Client,
    lat: f64,
    lon: f64,
    timezone: &str,
    month: &str,
) -> anyhow::Result<Option<WeatherMetrics>> {
    let Some(start) = month_start(month) else {
        return Ok(None);
    };
    let Some(end) = month_end(start) else {
        return Ok(None);
    };

    let response = client
        .get("https://archive-api.open-meteo.com/v1/archive")
        .query(&[
            ("latitude", lat.to_string()),
            ("longitude", lon.to_string()),
            ("start_date", start.to_string()),
            ("end_date", end.to_string()),
            ("daily", "temperature_2m_mean".to_string()),
            ("temperature_unit", "fahrenheit".to_string()),
            ("timezone", timezone.to_string()),
        ])
        .send()
        .await
        .context("fetching Open-Meteo archive data")?
        .error_for_status()
        .context("Open-Meteo returned an error")?
        .json::<OpenMeteoResponse>()
        .await
        .context("parsing Open-Meteo archive response")?;

    let temps = response.daily.temperature_2m_mean;
    if temps.is_empty() {
        return Ok(None);
    }

    let avg_temp_f = temps.iter().sum::<f64>() / temps.len() as f64;
    let heating_degree_days = temps.iter().map(|temp| (65.0 - temp).max(0.0)).sum();
    let cooling_degree_days = temps.iter().map(|temp| (temp - 65.0).max(0.0)).sum();

    Ok(Some(WeatherMetrics {
        avg_temp_f,
        heating_degree_days,
        cooling_degree_days,
    }))
}

fn month_start(month: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").ok()
}

fn month_end(start: NaiveDate) -> Option<NaiveDate> {
    let (year, month) = if start.month() == 12 {
        (start.year() + 1, 1)
    } else {
        (start.year(), start.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1).map(|date| date - Duration::days(1))
}
