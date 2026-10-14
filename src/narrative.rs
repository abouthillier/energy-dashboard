use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};

use crate::config::Config;

#[derive(Debug, Clone, Serialize)]
pub struct MonthFacts {
    pub bill_month: String,
    pub incomplete: bool,
    pub electric_kwh: Option<f64>,
    pub gas_therms: Option<f64>,
    pub total_cost: Option<f64>,
    pub avg_temp_f: Option<f64>,
    pub vs_prior_month: Comparisons,
    pub vs_same_month_last_year: Comparisons,
    pub vs_same_month_history_avg: Comparisons,
    pub records: RecordHints,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Comparisons {
    pub electric_kwh_pct: Option<f64>,
    pub gas_therms_pct: Option<f64>,
    pub total_cost_pct: Option<f64>,
    pub avg_temp_f_delta: Option<f64>,
    pub baseline_electric_kwh: Option<f64>,
    pub baseline_gas_therms: Option<f64>,
    pub baseline_avg_temp_f: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RecordHints {
    pub electric_near_low: bool,
    pub electric_near_high: bool,
    pub gas_near_low: bool,
    pub gas_near_high: bool,
}

#[derive(Debug, Serialize)]
pub struct NarrativeResponse {
    pub bill_month: String,
    pub text: String,
    pub source: String,
    pub model: Option<String>,
    pub cached: bool,
    pub facts: MonthFacts,
}

#[derive(Debug, Deserialize)]
struct OllamaGenerateResponse {
    response: String,
}

#[derive(Debug, Clone)]
struct MonthRow {
    bill_month: String,
    total_cost: f64,
    electric_kwh: Option<f64>,
    gas_therms: Option<f64>,
    avg_temp_f: Option<f64>,
}

pub async fn get_narrative(
    pool: &SqlitePool,
    config: &Config,
    force: bool,
) -> anyhow::Result<NarrativeResponse> {
    let facts = build_facts(pool).await?;
    let facts_hash = hash_facts(&facts);

    if !force {
        if let Some(cached) = load_cached(pool, &facts.bill_month, &facts_hash).await? {
            return Ok(NarrativeResponse {
                bill_month: facts.bill_month.clone(),
                text: cached.0,
                source: cached.1,
                model: cached.2,
                cached: true,
                facts,
            });
        }
    }

    let (text, source, model) = match generate_with_ollama(config, &facts).await {
        Ok(text) => (text, "ollama".to_string(), Some(config.ollama_model.clone())),
        Err(error) => {
            tracing::warn!("ollama narrative failed, using fallback: {error:#}");
            (fallback_text(&facts), "fallback".to_string(), None)
        }
    };

    save_cached(pool, &facts.bill_month, &facts_hash, &text, &source, model.as_deref()).await?;

    Ok(NarrativeResponse {
        bill_month: facts.bill_month.clone(),
        text,
        source,
        model,
        cached: false,
        facts,
    })
}

async fn build_facts(pool: &SqlitePool) -> anyhow::Result<MonthFacts> {
    let rows = sqlx::query(
        r#"
        SELECT b.bill_month,
               SUM(b.total_cost) AS total_cost,
               SUM(b.electric_kwh) AS electric_kwh,
               SUM(b.gas_therms) AS gas_therms,
               w.avg_temp_f
        FROM utility_bills b
        LEFT JOIN weather_monthly w ON w.month = b.bill_month
        GROUP BY b.bill_month
        ORDER BY b.bill_month
        "#,
    )
    .fetch_all(pool)
    .await?;

    let months: Vec<MonthRow> = rows
        .iter()
        .map(|row| MonthRow {
            bill_month: row.get("bill_month"),
            total_cost: row.get("total_cost"),
            electric_kwh: opt_f64(row, "electric_kwh"),
            gas_therms: opt_f64(row, "gas_therms"),
            avg_temp_f: opt_f64(row, "avg_temp_f"),
        })
        .collect();

    let latest = months
        .last()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no monthly bills available"))?;
    let prior = months
        .iter()
        .rev()
        .nth(1)
        .cloned();
    let same_month_last_year = months
        .iter()
        .find(|row| row.bill_month == prior_year_month(&latest.bill_month))
        .cloned();

    let month_suffix = &latest.bill_month[5..];
    let history: Vec<&MonthRow> = months
        .iter()
        .filter(|row| row.bill_month < latest.bill_month && row.bill_month.ends_with(month_suffix))
        .collect();

    let mut notes = Vec::new();
    let incomplete = latest.electric_kwh.is_none() || latest.gas_therms.is_none();
    if latest.electric_kwh.is_none() {
        notes.push("Electric usage for this month is not imported yet.".to_string());
    }
    if latest.gas_therms.is_none() {
        notes.push("Gas usage for this month is not imported yet.".to_string());
    }
    if latest.avg_temp_f.is_none() {
        notes.push("Average outdoor temperature for this month is not available yet.".to_string());
    }

    let vs_prior_month = compare_months(Some(&latest), prior.as_ref());
    let vs_same_month_last_year = compare_months(Some(&latest), same_month_last_year.as_ref());
    let vs_same_month_history_avg = compare_to_average(&latest, &history);

    let records = record_hints(&latest, &months);

    Ok(MonthFacts {
        bill_month: latest.bill_month,
        incomplete,
        electric_kwh: latest.electric_kwh,
        gas_therms: latest.gas_therms,
        total_cost: Some(latest.total_cost),
        avg_temp_f: latest.avg_temp_f,
        vs_prior_month,
        vs_same_month_last_year,
        vs_same_month_history_avg,
        records,
        notes,
    })
}

fn compare_months(current: Option<&MonthRow>, baseline: Option<&MonthRow>) -> Comparisons {
    let Some(current) = current else {
        return Comparisons::default();
    };
    let Some(baseline) = baseline else {
        return Comparisons::default();
    };

    Comparisons {
        electric_kwh_pct: pct_change(current.electric_kwh, baseline.electric_kwh),
        gas_therms_pct: pct_change(current.gas_therms, baseline.gas_therms),
        total_cost_pct: pct_change(Some(current.total_cost), Some(baseline.total_cost)),
        avg_temp_f_delta: match (current.avg_temp_f, baseline.avg_temp_f) {
            (Some(a), Some(b)) => Some(a - b),
            _ => None,
        },
        baseline_electric_kwh: baseline.electric_kwh,
        baseline_gas_therms: baseline.gas_therms,
        baseline_avg_temp_f: baseline.avg_temp_f,
    }
}

fn compare_to_average(current: &MonthRow, history: &[&MonthRow]) -> Comparisons {
    if history.is_empty() {
        return Comparisons::default();
    }

    let avg_electric = average(history.iter().filter_map(|row| row.electric_kwh));
    let avg_gas = average(history.iter().filter_map(|row| row.gas_therms));
    let avg_temp = average(history.iter().filter_map(|row| row.avg_temp_f));
    let avg_cost = average(history.iter().map(|row| row.total_cost));

    Comparisons {
        electric_kwh_pct: pct_change(current.electric_kwh, avg_electric),
        gas_therms_pct: pct_change(current.gas_therms, avg_gas),
        total_cost_pct: pct_change(Some(current.total_cost), avg_cost),
        avg_temp_f_delta: match (current.avg_temp_f, avg_temp) {
            (Some(a), Some(b)) => Some(a - b),
            _ => None,
        },
        baseline_electric_kwh: avg_electric,
        baseline_gas_therms: avg_gas,
        baseline_avg_temp_f: avg_temp,
    }
}

fn record_hints(latest: &MonthRow, months: &[MonthRow]) -> RecordHints {
    let electric_values: Vec<f64> = months.iter().filter_map(|row| row.electric_kwh).filter(|v| *v > 0.0).collect();
    let gas_values: Vec<f64> = months.iter().filter_map(|row| row.gas_therms).filter(|v| *v > 0.0).collect();

    let (e_min, e_max) = min_max(&electric_values);
    let (g_min, g_max) = min_max(&gas_values);

    RecordHints {
        electric_near_low: near_extreme(latest.electric_kwh, e_min, true),
        electric_near_high: near_extreme(latest.electric_kwh, e_max, false),
        gas_near_low: near_extreme(latest.gas_therms, g_min, true),
        gas_near_high: near_extreme(latest.gas_therms, g_max, false),
    }
}

fn near_extreme(value: Option<f64>, extreme: Option<f64>, is_low: bool) -> bool {
    match (value, extreme) {
        (Some(value), Some(extreme)) if extreme > 0.0 => {
            let ratio = if is_low {
                (value - extreme) / extreme
            } else {
                (extreme - value) / extreme
            };
            ratio >= 0.0 && ratio <= 0.08
        }
        _ => false,
    }
}

fn min_max(values: &[f64]) -> (Option<f64>, Option<f64>) {
    if values.is_empty() {
        return (None, None);
    }
    let mut min = values[0];
    let mut max = values[0];
    for value in values.iter().copied().skip(1) {
        min = min.min(value);
        max = max.max(value);
    }
    (Some(min), Some(max))
}

fn average(values: impl Iterator<Item = f64>) -> Option<f64> {
    let values: Vec<f64> = values.filter(|value| value.is_finite() && *value > 0.0).collect();
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

fn pct_change(current: Option<f64>, previous: Option<f64>) -> Option<f64> {
    match (current, previous) {
        (Some(current), Some(previous)) if previous != 0.0 && current.is_finite() && previous.is_finite() => {
            Some(((current - previous) / previous) * 100.0)
        }
        _ => None,
    }
}

fn prior_year_month(bill_month: &str) -> String {
    let year: i32 = bill_month[0..4].parse().unwrap_or(0);
    format!("{:04}{}", year - 1, &bill_month[4..])
}

fn hash_facts(facts: &MonthFacts) -> String {
    let payload = serde_json::to_vec(facts).unwrap_or_default();
    let digest = Sha256::digest(payload);
    hex::encode(digest)
}

async fn load_cached(
    pool: &SqlitePool,
    bill_month: &str,
    facts_hash: &str,
) -> anyhow::Result<Option<(String, String, Option<String>)>> {
    let row = sqlx::query(
        r#"
        SELECT text, source, model
        FROM month_narratives
        WHERE bill_month = ? AND facts_hash = ?
        "#,
    )
    .bind(bill_month)
    .bind(facts_hash)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| {
        (
            row.get("text"),
            row.get("source"),
            row.try_get::<Option<String>, _>("model").ok().flatten(),
        )
    }))
}

async fn save_cached(
    pool: &SqlitePool,
    bill_month: &str,
    facts_hash: &str,
    text: &str,
    source: &str,
    model: Option<&str>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO month_narratives (bill_month, facts_hash, text, source, model)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(bill_month) DO UPDATE SET
          facts_hash = excluded.facts_hash,
          text = excluded.text,
          source = excluded.source,
          model = excluded.model,
          created_at = datetime('now')
        "#,
    )
    .bind(bill_month)
    .bind(facts_hash)
    .bind(text)
    .bind(source)
    .bind(model)
    .execute(pool)
    .await?;
    Ok(())
}

async fn generate_with_ollama(config: &Config, facts: &MonthFacts) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()?;

    let prompt = format!(
        "Write 2-3 plain sentences summarizing this home energy billing month for a private dashboard.\n\
         Facts (JSON):\n{}\n\n\
         Rules:\n\
         - Use only these facts; never invent weather, causes, or missing metrics.\n\
         - Prefer year-over-year and seasonal comparisons when present.\n\
         - If incomplete or notes say electric/gas/temp is missing, say that plainly.\n\
         - No lists, labels, markdown, percentages in parentheses, or openers like \"Here's a summary\".\n\
         - Style example: \"Gas use over the last month is lower than this month last year. The electric bill is not in yet.\"\n\
         Reply with the sentences only.",
        serde_json::to_string_pretty(facts)?
    );

    let body = json!({
        "model": config.ollama_model,
        "prompt": prompt,
        "stream": false,
        "options": {
            "temperature": 0.35,
            "num_predict": 160
        }
    });

    let response = client
        .post(format!("{}/api/generate", config.ollama_url.trim_end_matches('/')))
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json::<OllamaGenerateResponse>()
        .await?;

    let text = clean_narrative(&response.response);
    if text.is_empty() {
        anyhow::bail!("ollama returned empty narrative");
    }
    Ok(text)
}

fn clean_narrative(raw: &str) -> String {
    let mut text = raw.chars()
        .filter(|ch| *ch != '*' && *ch != '#')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .trim_matches('"')
        .to_string();

    for prefix in [
        "Here's a summary of the home energy bill:",
        "Here's a summary:",
        "Summary:",
        "Here is a summary:",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim().to_string();
        }
    }

    text
}

pub fn fallback_text(facts: &MonthFacts) -> String {
    let month = friendly_month(&facts.bill_month);
    let mut parts = Vec::new();

    if let Some(pct) = facts.vs_same_month_last_year.gas_therms_pct {
        parts.push(format!(
            "Gas use in {month} is {} than this month last year{}.",
            desc_pct(pct),
            if facts.records.gas_near_low {
                ", near a record low"
            } else if facts.records.gas_near_high {
                ", near a record high"
            } else {
                ""
            }
        ));
    } else if let Some(therms) = facts.gas_therms {
        parts.push(format!("Gas use in {month} was {therms:.0} therms."));
    }

    if let Some(pct) = facts.vs_same_month_last_year.electric_kwh_pct {
        let weather = weather_clause(facts);
        parts.push(format!(
            "Electric use is {} than last year{weather}.",
            desc_pct(pct)
        ));
    } else if facts.electric_kwh.is_none() {
        parts.push(format!("The electric bill for {month} is not in yet."));
    }

    if parts.is_empty() {
        parts.push(format!("Not enough history yet to summarize {month}."));
    }

    for note in &facts.notes {
        if !parts.iter().any(|part| part.contains("not in yet") || part.contains("not imported")) {
            if note.contains("Electric") {
                parts.push(note.clone());
            }
        }
    }

    parts.join(" ")
}

fn weather_clause(facts: &MonthFacts) -> String {
    match (
        facts.avg_temp_f,
        facts.vs_same_month_history_avg.avg_temp_f_delta,
    ) {
        (Some(temp), Some(delta)) if delta.abs() >= 2.0 => {
            let direction = if delta > 0.0 { "warmer" } else { "cooler" };
            format!(
                ", with outdoor temps around {temp:.0}°F ({direction} than the usual for this month)"
            )
        }
        (Some(temp), _) => format!(", with outdoor temps around {temp:.0}°F"),
        _ => String::new(),
    }
}

fn desc_pct(pct: f64) -> String {
    let abs = pct.abs();
    if abs < 3.0 {
        "about even".to_string()
    } else if pct < 0.0 {
        format!("about {abs:.0}% lower")
    } else {
        format!("about {abs:.0}% higher")
    }
}

fn friendly_month(bill_month: &str) -> String {
    let Ok(date) = chrono::NaiveDate::parse_from_str(&format!("{bill_month}-01"), "%Y-%m-%d") else {
        return bill_month.to_string();
    };
    date.format("%B %Y").to_string()
}

fn opt_f64(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<f64> {
    row.try_get::<Option<f64>, _>(column).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_mentions_missing_electric_and_yoy_gas() {
        let facts = MonthFacts {
            bill_month: "2026-07".into(),
            incomplete: true,
            electric_kwh: None,
            gas_therms: Some(17.0),
            total_cost: Some(62.74),
            avg_temp_f: None,
            vs_prior_month: Comparisons::default(),
            vs_same_month_last_year: Comparisons {
                gas_therms_pct: Some(-19.05),
                ..Comparisons::default()
            },
            vs_same_month_history_avg: Comparisons::default(),
            records: RecordHints::default(),
            notes: vec!["Electric usage for this month is not imported yet.".into()],
        };

        let text = fallback_text(&facts);
        assert!(text.contains("Gas use"));
        assert!(text.contains("lower"));
        assert!(text.contains("electric bill") || text.contains("Electric"));
    }

    #[test]
    fn pct_change_handles_none() {
        assert_eq!(pct_change(Some(110.0), Some(100.0)), Some(10.0));
        assert_eq!(pct_change(None, Some(100.0)), None);
    }
}
