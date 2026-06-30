use std::{collections::HashMap, path::{Path, PathBuf}, process::Stdio};

use anyhow::{bail, Context};
use chrono::{Datelike, NaiveDate};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use tokio::{fs, process::Command};

use crate::{
    config::Config,
    models::{ImportRun, UtilityBill},
    parsers,
};

pub async fn import_invoice_dir(pool: &SqlitePool, config: &Config) -> anyhow::Result<ImportRun> {
    let run_id = create_import_run(pool, "invoice-folder").await?;
    let mut files_seen = 0;
    let mut files_imported = 0;
    let mut files_skipped = 0;
    let mut errors = 0;

    let mut dir = match fs::read_dir(&config.invoice_dir).await {
        Ok(dir) => dir,
        Err(error) => {
            finish_import_run(pool, run_id, "error", files_seen, files_imported, files_skipped, 1, Some(error.to_string())).await?;
            return get_import_run(pool, run_id).await;
        }
    };

    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();
        if !is_supported_invoice_file(&path) {
            continue;
        }

        files_seen += 1;
        match import_invoice_file(pool, &path).await {
            Ok(ImportFileOutcome::Imported) => files_imported += 1,
            Ok(ImportFileOutcome::Skipped) => files_skipped += 1,
            Err(error) => {
                errors += 1;
                tracing::warn!("failed importing {}: {error:?}", path.display());
            }
        }
    }

    let status = if errors > 0 { "partial" } else { "ok" };
    finish_import_run(pool, run_id, status, files_seen, files_imported, files_skipped, errors, None).await?;
    get_import_run(pool, run_id).await
}

pub async fn import_csv_file(pool: &SqlitePool, path: &Path) -> anyhow::Result<ImportRun> {
    let run_id = create_import_run(pool, "csv-backfill").await?;
    let result = async {
        let bytes = fs::read(path).await.with_context(|| format!("reading {}", path.display()))?;
        let sha = sha256_hex(&bytes);
        let source_id = ensure_source_file(pool, path, &sha, "csv", "historical-csv", "imported", None, None).await?;
        let content = String::from_utf8_lossy(&bytes);
        let rows = if path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.contains("Home Energy Usage"))
            .unwrap_or(false)
        {
            parse_home_energy_spreadsheet_csv(&content)?
        } else {
            parse_generic_csv(bytes.as_slice())?
        };
        let mut imported = 0;

        for (mut bill, avg_temp_f) in rows {
            if bill.provider == "Historical CSV" {
                let bill_month = bill.bill_month.clone();
                let has_electric = month_has_non_historical_metric(pool, &bill_month, "electric").await?;
                let has_gas = month_has_non_historical_metric(pool, &bill_month, "gas").await?;
                let Some(trimmed) = trim_historical_bill(bill, has_electric, has_gas) else {
                    if let Some(avg_temp_f) = avg_temp_f {
                        upsert_historical_weather(pool, &bill_month, avg_temp_f).await?;
                    }
                    continue;
                };
                bill = trimmed;
            }
            bill.source_file_id = Some(source_id);
            upsert_bill(pool, &bill).await?;
            if let Some(avg_temp_f) = avg_temp_f {
                upsert_historical_weather(pool, &bill.bill_month, avg_temp_f).await?;
            }
            imported += 1;
        }

        anyhow::Ok(imported)
    }
    .await;

    match result {
        Ok(imported) => {
            finish_import_run(pool, run_id, "ok", 1, imported, 0, 0, None).await?;
        }
        Err(error) => {
            finish_import_run(pool, run_id, "error", 1, 0, 0, 1, Some(error.to_string())).await?;
        }
    }

    get_import_run(pool, run_id).await
}

enum ImportFileOutcome {
    Imported,
    Skipped,
}

async fn import_invoice_file(pool: &SqlitePool, path: &Path) -> anyhow::Result<ImportFileOutcome> {
    let bytes = fs::read(path).await?;
    let sha = sha256_hex(&bytes);
    if source_exists(pool, &sha).await? {
        return Ok(ImportFileOutcome::Skipped);
    }

    let text = extract_text(path).await?;
    match parsers::parse_invoice(&text) {
        Ok(mut bill) => {
            let source_id = ensure_source_file(pool, path, &sha, "invoice", &bill.provider, "imported", Some(&text), None).await?;
            bill.source_file_id = Some(source_id);
            upsert_bill(pool, &bill).await?;
            Ok(ImportFileOutcome::Imported)
        }
        Err(error) => {
            ensure_source_file(pool, path, &sha, "invoice", "unknown", "error", Some(&text), Some(&error.to_string())).await?;
            bail!(error)
        }
    }
}

async fn extract_text(path: &Path) -> anyhow::Result<String> {
    let output = Command::new("pdftotext")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| "running pdftotext; install poppler-utils in the container/host")?;

    if !output.status.success() {
        bail!(
            "pdftotext failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

async fn source_exists(pool: &SqlitePool, sha: &str) -> anyhow::Result<bool> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_files WHERE sha256 = ?")
        .bind(sha)
        .fetch_one(pool)
        .await?;
    Ok(count > 0)
}

async fn ensure_source_file(
    pool: &SqlitePool,
    path: &Path,
    sha: &str,
    kind: &str,
    provider: &str,
    status: &str,
    extracted_text: Option<&str>,
    error: Option<&str>,
) -> anyhow::Result<i64> {
    sqlx::query(
        r#"
        INSERT INTO source_files (path, filename, sha256, kind, provider, status, extracted_text, error)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(sha256) DO UPDATE SET
          status = excluded.status,
          extracted_text = excluded.extracted_text,
          error = excluded.error,
          provider = excluded.provider
        "#,
    )
    .bind(path.to_string_lossy().to_string())
    .bind(path.file_name().and_then(|value| value.to_str()).unwrap_or("unknown"))
    .bind(sha)
    .bind(kind)
    .bind(provider)
    .bind(status)
    .bind(extracted_text)
    .bind(error)
    .execute(pool)
    .await?;

    let id: i64 = sqlx::query_scalar("SELECT id FROM source_files WHERE sha256 = ?")
        .bind(sha)
        .fetch_one(pool)
        .await?;
    Ok(id)
}

pub async fn upsert_bill(pool: &SqlitePool, bill: &UtilityBill) -> anyhow::Result<()> {
    if bill.provider != "Historical CSV" {
        prune_historical_bill(pool, bill).await?;
    }

    sqlx::query(
        r#"
        INSERT INTO utility_bills (
          bill_month, provider, account_label, billing_period_start, billing_period_end,
          bill_date, due_date, service_days, electric_kwh, gas_therms, total_cost,
          electric_cost, gas_cost, supply_cost, delivery_cost, public_benefits_cost,
          credits_cost, source_file_id, parser_confidence
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(provider, billing_period_start, billing_period_end) DO UPDATE SET
          bill_month = excluded.bill_month,
          account_label = excluded.account_label,
          bill_date = excluded.bill_date,
          due_date = excluded.due_date,
          service_days = excluded.service_days,
          electric_kwh = excluded.electric_kwh,
          gas_therms = excluded.gas_therms,
          total_cost = excluded.total_cost,
          electric_cost = excluded.electric_cost,
          gas_cost = excluded.gas_cost,
          supply_cost = excluded.supply_cost,
          delivery_cost = excluded.delivery_cost,
          public_benefits_cost = excluded.public_benefits_cost,
          credits_cost = excluded.credits_cost,
          source_file_id = excluded.source_file_id,
          parser_confidence = excluded.parser_confidence,
          updated_at = datetime('now')
        "#,
    )
    .bind(&bill.bill_month)
    .bind(&bill.provider)
    .bind(&bill.account_label)
    .bind(bill.billing_period_start.map(|date| date.to_string()))
    .bind(bill.billing_period_end.map(|date| date.to_string()))
    .bind(bill.bill_date.map(|date| date.to_string()))
    .bind(bill.due_date.map(|date| date.to_string()))
    .bind(bill.service_days)
    .bind(bill.electric_kwh)
    .bind(bill.gas_therms)
    .bind(bill.total_cost)
    .bind(bill.electric_cost)
    .bind(bill.gas_cost)
    .bind(bill.supply_cost)
    .bind(bill.delivery_cost)
    .bind(bill.public_benefits_cost)
    .bind(bill.credits_cost)
    .bind(bill.source_file_id)
    .bind(bill.parser_confidence)
    .execute(pool)
    .await?;

    Ok(())
}

async fn month_has_non_historical_metric(pool: &SqlitePool, bill_month: &str, metric: &str) -> anyhow::Result<bool> {
    let predicate = match metric {
        "electric" => "(electric_kwh IS NOT NULL OR electric_cost IS NOT NULL)",
        "gas" => "(gas_therms IS NOT NULL OR gas_cost IS NOT NULL)",
        _ => bail!("unknown metric {metric}"),
    };
    let sql = format!(
        "SELECT COUNT(*) FROM utility_bills WHERE bill_month = ? AND provider != 'Historical CSV' AND {predicate}",
    );
    let count: i64 = sqlx::query_scalar(
        &sql,
    )
    .bind(bill_month)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

async fn prune_historical_bill(pool: &SqlitePool, bill: &UtilityBill) -> anyhow::Result<()> {
    let Some(row) = sqlx::query(
        r#"
        SELECT id, bill_month, provider, account_label, billing_period_start, billing_period_end,
               bill_date, due_date, service_days, electric_kwh, gas_therms, total_cost,
               electric_cost, gas_cost, supply_cost, delivery_cost, public_benefits_cost,
               credits_cost, source_file_id, parser_confidence
        FROM utility_bills
        WHERE provider = 'Historical CSV' AND bill_month = ?
        "#,
    )
    .bind(&bill.bill_month)
    .fetch_optional(pool)
    .await? else {
        return Ok(());
    };

    let historical = UtilityBill {
        id: Some(row.get("id")),
        bill_month: row.get("bill_month"),
        provider: row.get("provider"),
        account_label: row.try_get::<Option<String>, _>("account_label").ok().flatten(),
        billing_period_start: row
            .try_get::<Option<String>, _>("billing_period_start")
            .ok()
            .flatten()
            .and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok()),
        billing_period_end: row
            .try_get::<Option<String>, _>("billing_period_end")
            .ok()
            .flatten()
            .and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok()),
        bill_date: row
            .try_get::<Option<String>, _>("bill_date")
            .ok()
            .flatten()
            .and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok()),
        due_date: row
            .try_get::<Option<String>, _>("due_date")
            .ok()
            .flatten()
            .and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok()),
        service_days: row.try_get::<Option<i64>, _>("service_days").ok().flatten(),
        electric_kwh: row.try_get::<Option<f64>, _>("electric_kwh").ok().flatten(),
        gas_therms: row.try_get::<Option<f64>, _>("gas_therms").ok().flatten(),
        total_cost: row.get("total_cost"),
        electric_cost: row.try_get::<Option<f64>, _>("electric_cost").ok().flatten(),
        gas_cost: row.try_get::<Option<f64>, _>("gas_cost").ok().flatten(),
        supply_cost: row.try_get::<Option<f64>, _>("supply_cost").ok().flatten(),
        delivery_cost: row.try_get::<Option<f64>, _>("delivery_cost").ok().flatten(),
        public_benefits_cost: row.try_get::<Option<f64>, _>("public_benefits_cost").ok().flatten(),
        credits_cost: row.try_get::<Option<f64>, _>("credits_cost").ok().flatten(),
        source_file_id: row.try_get::<Option<i64>, _>("source_file_id").ok().flatten(),
        parser_confidence: row.get("parser_confidence"),
    };

    let has_electric = has_electric_data(bill);
    let has_gas = has_gas_data(bill);
    if let Some(trimmed) = trim_historical_bill(historical, has_electric, has_gas) {
        update_historical_bill(pool, &trimmed).await?;
    } else {
        sqlx::query("DELETE FROM utility_bills WHERE provider = 'Historical CSV' AND bill_month = ?")
            .bind(&bill.bill_month)
            .execute(pool)
            .await?;
    }

    Ok(())
}

async fn update_historical_bill(pool: &SqlitePool, bill: &UtilityBill) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE utility_bills
        SET electric_kwh = ?, gas_therms = ?, total_cost = ?,
            electric_cost = ?, gas_cost = ?, supply_cost = ?, delivery_cost = ?,
            public_benefits_cost = ?, credits_cost = ?, updated_at = datetime('now')
        WHERE id = ? AND provider = 'Historical CSV'
        "#,
    )
    .bind(bill.electric_kwh)
    .bind(bill.gas_therms)
    .bind(bill.total_cost)
    .bind(bill.electric_cost)
    .bind(bill.gas_cost)
    .bind(bill.supply_cost)
    .bind(bill.delivery_cost)
    .bind(bill.public_benefits_cost)
    .bind(bill.credits_cost)
    .bind(bill.id)
    .execute(pool)
    .await?;
    Ok(())
}

fn trim_historical_bill(mut bill: UtilityBill, has_electric_replacement: bool, has_gas_replacement: bool) -> Option<UtilityBill> {
    let original_total = bill.total_cost;
    let keep_electric = has_electric_data(&bill) && !has_electric_replacement;
    let keep_gas = has_gas_data(&bill) && !has_gas_replacement;

    if !keep_electric {
        bill.electric_kwh = None;
        bill.electric_cost = None;
    }
    if !keep_gas {
        bill.gas_therms = None;
        bill.gas_cost = None;
    }
    if !keep_electric && !keep_gas {
        return None;
    }

    let kept_costs = [bill.electric_cost, bill.gas_cost]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    bill.total_cost = if kept_costs.is_empty() {
        if keep_electric && keep_gas { original_total } else { 0.0 }
    } else {
        kept_costs.into_iter().sum()
    };

    Some(bill)
}

fn has_electric_data(bill: &UtilityBill) -> bool {
    bill.electric_kwh.is_some() || bill.electric_cost.is_some()
}

fn has_gas_data(bill: &UtilityBill) -> bool {
    bill.gas_therms.is_some() || bill.gas_cost.is_some()
}

async fn create_import_run(pool: &SqlitePool, source: &str) -> anyhow::Result<i64> {
    let result = sqlx::query("INSERT INTO import_runs (source, status) VALUES (?, 'running')")
        .bind(source)
        .execute(pool)
        .await?;
    Ok(result.last_insert_rowid())
}

async fn finish_import_run(
    pool: &SqlitePool,
    run_id: i64,
    status: &str,
    files_seen: i64,
    files_imported: i64,
    files_skipped: i64,
    errors: i64,
    message: Option<String>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE import_runs
        SET finished_at = datetime('now'), status = ?, files_seen = ?,
            files_imported = ?, files_skipped = ?, errors = ?, message = ?
        WHERE id = ?
        "#,
    )
    .bind(status)
    .bind(files_seen)
    .bind(files_imported)
    .bind(files_skipped)
    .bind(errors)
    .bind(message)
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn get_import_run(pool: &SqlitePool, run_id: i64) -> anyhow::Result<ImportRun> {
    let row = sqlx::query(
        r#"
        SELECT id, started_at, finished_at, source, status, files_seen,
               files_imported, files_skipped, errors, message
        FROM import_runs
        WHERE id = ?
        "#,
    )
    .bind(run_id)
    .fetch_one(pool)
    .await?;

    Ok(ImportRun {
        id: row.get("id"),
        started_at: row.get("started_at"),
        finished_at: row.try_get::<Option<String>, _>("finished_at").ok().flatten(),
        source: row.get("source"),
        status: row.get("status"),
        files_seen: row.get("files_seen"),
        files_imported: row.get("files_imported"),
        files_skipped: row.get("files_skipped"),
        errors: row.get("errors"),
        message: row.try_get::<Option<String>, _>("message").ok().flatten(),
    })
}

fn parse_generic_csv(bytes: &[u8]) -> anyhow::Result<Vec<(UtilityBill, Option<f64>)>> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(bytes);
    let headers = reader.headers()?.clone();
    let mut bills = Vec::new();

    for record in reader.records() {
        let record = record?;
        let row = headers
            .iter()
            .zip(record.iter())
            .map(|(header, value)| (normalize_header(header), value.trim().to_string()))
            .collect::<HashMap<_, _>>();

        if let Some(bill) = bill_from_csv_row(&row) {
            bills.push((bill, None));
        }
    }

    Ok(bills)
}

fn parse_home_energy_spreadsheet_csv(content: &str) -> anyhow::Result<Vec<(UtilityBill, Option<f64>)>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(content.as_bytes());
    let mut bills = Vec::new();

    for record in reader.records() {
        let record = record?;
        let service = record.get(2).unwrap_or("").trim();
        if service.is_empty() || service.eq_ignore_ascii_case("service") {
            continue;
        }

        let Some(bill_month) = normalize_month(service) else {
            continue;
        };
        let electric_cost = record.get(3).map(str::trim).and_then(parse_f64_str);
        let electric_kwh = record.get(4).map(str::trim).and_then(parse_f64_str);
        let gas_cost = record.get(7).map(str::trim).and_then(parse_f64_str);
        let gas_therms = record.get(8).map(str::trim).and_then(parse_f64_str);
        let total_cost = record
            .get(11)
            .map(str::trim)
            .and_then(parse_f64_str)
            .or_else(|| Some(electric_cost.unwrap_or(0.0) + gas_cost.unwrap_or(0.0)));
        let avg_temp_f = record.get(12).map(str::trim).and_then(parse_f64_str);

        let Some(total_cost) = total_cost.filter(|value| *value > 0.0) else {
            continue;
        };

        let start = month_start(&bill_month);
        let end = month_end(&bill_month);
        bills.push((
            UtilityBill {
                id: None,
                bill_month,
                provider: "Historical CSV".to_string(),
                account_label: None,
                billing_period_start: start,
                billing_period_end: end,
                bill_date: end,
                due_date: None,
                service_days: None,
                electric_kwh,
                gas_therms,
                total_cost,
                electric_cost,
                gas_cost,
                supply_cost: None,
                delivery_cost: None,
                public_benefits_cost: None,
                credits_cost: None,
                source_file_id: None,
                parser_confidence: 0.75,
            },
            avg_temp_f,
        ));
    }

    Ok(bills)
}

async fn upsert_historical_weather(pool: &SqlitePool, month: &str, avg_temp_f: f64) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO weather_monthly (month, avg_temp_f, source)
        VALUES (?, ?, 'historical-csv')
        ON CONFLICT(month) DO UPDATE SET
          avg_temp_f = excluded.avg_temp_f,
          source = excluded.source,
          fetched_at = datetime('now')
        "#,
    )
    .bind(month)
    .bind(avg_temp_f)
    .execute(pool)
    .await?;
    Ok(())
}

fn bill_from_csv_row(row: &HashMap<String, String>) -> Option<UtilityBill> {
    let month = get_any(row, &["month", "bill month", "date", "billing month"])?;
    let bill_month = normalize_month(month)?;
    let electric_kwh = get_any(row, &["kwh", "kw h", "electric kwh", "electricity kwh"]).and_then(parse_f64);
    let gas_therms = get_any(row, &["therms", "gas therms"]).and_then(parse_f64);
    let total_cost = get_any(row, &["total cost", "cost", "amount", "total"]).and_then(parse_f64)?;
    let electric_cost = get_any(row, &["electric cost", "electricity cost"]).and_then(parse_f64);
    let gas_cost = get_any(row, &["gas cost"]).and_then(parse_f64);
    let provider = get_any(row, &["provider", "utility"]).cloned().unwrap_or_else(|| "Historical CSV".to_string());
    let start = month_start(&bill_month);
    let end = month_end(&bill_month);

    Some(UtilityBill {
        id: None,
        bill_month,
        provider,
        account_label: None,
        billing_period_start: start,
        billing_period_end: end,
        bill_date: end,
        due_date: None,
        service_days: None,
        electric_kwh,
        gas_therms,
        total_cost,
        electric_cost,
        gas_cost,
        supply_cost: None,
        delivery_cost: None,
        public_benefits_cost: None,
        credits_cost: None,
        source_file_id: None,
        parser_confidence: 0.75,
    })
}

fn get_any<'a>(row: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a String> {
    keys.iter().find_map(|key| row.get(*key)).filter(|value| !value.trim().is_empty())
}

fn normalize_header(header: &str) -> String {
    header.trim().to_lowercase().replace('_', " ")
}

fn normalize_month(value: &str) -> Option<String> {
    let value = value.trim().replace("Sept", "Sep").replace('\'', "");
    if RegexLite::is_yyyy_mm(&value) {
        return Some(value[..7].to_string());
    }

    if let Some(month) = parse_named_short_month(&value) {
        return Some(month);
    }

    for format in ["%Y-%m-%d", "%m/%d/%Y", "%m/%d/%y", "%B %Y", "%b %Y"] {
        if let Ok(date) = NaiveDate::parse_from_str(&value, format) {
            return Some(format!("{:04}-{:02}", date.year(), date.month()));
        }
    }

    None
}

fn parse_named_short_month(value: &str) -> Option<String> {
    let mut parts = value.split_whitespace();
    let month_name = parts.next()?.to_lowercase();
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = match month_name.as_str() {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    };
    let full_year = if year < 100 { 2000 + year } else { year };
    Some(format!("{full_year:04}-{month:02}"))
}

fn month_start(month: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").ok()
}

fn month_end(month: &str) -> Option<NaiveDate> {
    let start = month_start(month)?;
    let (year, month) = if start.month() == 12 {
        (start.year() + 1, 1)
    } else {
        (start.year(), start.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1)?.pred_opt()
}

fn parse_f64(value: &String) -> Option<f64> {
    parse_f64_str(value)
}

fn parse_f64_str(value: &str) -> Option<f64> {
    let cleaned = value
        .replace(['$', ','], "")
        .replace('F', "")
        .replace('f', "");
    cleaned.trim().parse::<f64>().ok()
}

fn is_supported_invoice_file(path: &PathBuf) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_lowercase().as_str(), "pdf"))
        .unwrap_or(false)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

struct RegexLite;

impl RegexLite {
    fn is_yyyy_mm(value: &str) -> bool {
        value.len() >= 7
            && value.as_bytes().get(4) == Some(&b'-')
            && value[..4].chars().all(|char| char.is_ascii_digit())
            && value[5..7].chars().all(|char| char.is_ascii_digit())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn historical_bill() -> UtilityBill {
        UtilityBill {
            id: Some(1),
            bill_month: "2025-01".to_string(),
            provider: "Historical CSV".to_string(),
            account_label: None,
            billing_period_start: month_start("2025-01"),
            billing_period_end: month_end("2025-01"),
            bill_date: month_end("2025-01"),
            due_date: None,
            service_days: None,
            electric_kwh: Some(500.0),
            gas_therms: Some(100.0),
            total_cost: 300.0,
            electric_cost: Some(100.0),
            gas_cost: Some(200.0),
            supply_cost: None,
            delivery_cost: None,
            public_benefits_cost: None,
            credits_cost: None,
            source_file_id: Some(1),
            parser_confidence: 0.75,
        }
    }

    #[test]
    fn historical_csv_keeps_only_metrics_missing_from_real_bills() {
        let gas_only = trim_historical_bill(historical_bill(), true, false).expect("keeps gas");
        assert_eq!(gas_only.electric_kwh, None);
        assert_eq!(gas_only.electric_cost, None);
        assert_eq!(gas_only.gas_therms, Some(100.0));
        assert_eq!(gas_only.gas_cost, Some(200.0));
        assert_eq!(gas_only.total_cost, 200.0);

        let electric_only = trim_historical_bill(historical_bill(), false, true).expect("keeps electric");
        assert_eq!(electric_only.electric_kwh, Some(500.0));
        assert_eq!(electric_only.electric_cost, Some(100.0));
        assert_eq!(electric_only.gas_therms, None);
        assert_eq!(electric_only.gas_cost, None);
        assert_eq!(electric_only.total_cost, 100.0);

        assert!(trim_historical_bill(historical_bill(), true, true).is_none());
    }
}
