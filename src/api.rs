use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use sqlx::{Row, SqlitePool};

use crate::{
    config::Config,
    import,
    models::{CsvImportRequest, ImportRun, MonthlyAggregate, MonthlyBill, MonthlyQuery, SummaryResponse},
    weather,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Config,
}

#[derive(Debug)]
pub struct ApiError(anyhow::Error);

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(value: E) -> Self {
        Self(value.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::error!("{:?}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "error": self.0.to_string() })),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/energy", get(energy_index))
        .route("/energy/", get(energy_index))
        .route("/energy/summary", get(summary))
        .route("/energy/monthly", get(monthly))
        .route("/energy/import-runs", get(import_runs))
        .route("/energy/import", post(import_invoices))
        .route("/energy/import-csv", post(import_csv))
        .route(
            "/energy/upload",
            post(upload_invoice).layer(DefaultBodyLimit::max(UPLOAD_LIMIT_BYTES)),
        )
        .route("/energy/weather/enrich", post(enrich_weather))
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn energy_index() -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "endpoints": {
            "summary": "/energy/summary",
            "monthly": "/energy/monthly",
            "importRuns": "/energy/import-runs",
            "importInvoices": "POST /energy/import",
            "importCsv": "POST /energy/import-csv",
            "uploadInvoice": "POST /energy/upload?filename=<name.pdf|name.csv> (raw file body)",
            "enrichWeather": "POST /energy/weather/enrich"
        }
    }))
}

async fn summary(State(state): State<AppState>) -> ApiResult<Json<SummaryResponse>> {
    let latest = sqlx::query(
        r#"
        SELECT b.bill_month,
               SUM(b.total_cost) AS total_cost,
               SUM(b.electric_kwh) AS electric_kwh,
               SUM(b.gas_therms) AS gas_therms,
               w.avg_temp_f
        FROM utility_bills b
        LEFT JOIN weather_monthly w ON w.month = b.bill_month
        GROUP BY b.bill_month
        ORDER BY b.bill_month DESC
        LIMIT 1
        "#,
    )
    .fetch_optional(&state.pool)
    .await?;

    let ytd_cost: f64 = sqlx::query_scalar(
        r#"
        SELECT COALESCE(SUM(total_cost), 0.0)
        FROM utility_bills
        WHERE substr(bill_month, 1, 4) = strftime('%Y', 'now')
        "#,
    )
    .fetch_one(&state.pool)
    .await?;

    let imported_bill_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM utility_bills")
        .fetch_one(&state.pool)
        .await?;
    let failed_source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_files WHERE status = 'error'")
        .fetch_one(&state.pool)
        .await?;
    let last_import = latest_import_run(&state.pool).await?;

    Ok(Json(SummaryResponse {
        latest_month: latest.as_ref().map(|row| row.get("bill_month")),
        latest_total_cost: latest.as_ref().and_then(|row| opt_f64(row, "total_cost")),
        latest_electric_kwh: latest.as_ref().and_then(|row| opt_f64(row, "electric_kwh")),
        latest_gas_therms: latest.as_ref().and_then(|row| opt_f64(row, "gas_therms")),
        latest_avg_temp_f: latest.as_ref().and_then(|row| opt_f64(row, "avg_temp_f")),
        year_to_date_cost: ytd_cost,
        imported_bill_count,
        failed_source_count,
        last_import,
    }))
}

async fn monthly(
    State(state): State<AppState>,
    Query(query): Query<MonthlyQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let from = query.from.unwrap_or_else(|| "1900-01".to_string());
    let to = query.to.unwrap_or_else(|| "2999-12".to_string());

    let rows = sqlx::query(
        r#"
        SELECT b.id, b.bill_month, b.provider, b.billing_period_start, b.billing_period_end,
               b.bill_date, b.due_date, b.service_days, b.electric_kwh, b.gas_therms,
               b.total_cost, b.electric_cost, b.gas_cost, b.supply_cost, b.delivery_cost,
               b.public_benefits_cost, w.avg_temp_f, w.heating_degree_days, w.cooling_degree_days,
               sf.filename AS source_filename
        FROM utility_bills b
        LEFT JOIN weather_monthly w ON w.month = b.bill_month
        LEFT JOIN source_files sf ON sf.id = b.source_file_id
        WHERE b.bill_month BETWEEN ? AND ?
        ORDER BY b.bill_month DESC, b.provider
        "#,
    )
    .bind(&from)
    .bind(&to)
    .fetch_all(&state.pool)
    .await?;

    let bills = rows
        .iter()
        .map(|row| MonthlyBill {
            id: row.get("id"),
            bill_month: row.get("bill_month"),
            provider: row.get("provider"),
            billing_period_start: opt_string(row, "billing_period_start"),
            billing_period_end: opt_string(row, "billing_period_end"),
            bill_date: opt_string(row, "bill_date"),
            due_date: opt_string(row, "due_date"),
            service_days: opt_i64(row, "service_days"),
            electric_kwh: opt_f64(row, "electric_kwh"),
            gas_therms: opt_f64(row, "gas_therms"),
            total_cost: row.get("total_cost"),
            electric_cost: opt_f64(row, "electric_cost"),
            gas_cost: opt_f64(row, "gas_cost"),
            supply_cost: opt_f64(row, "supply_cost"),
            delivery_cost: opt_f64(row, "delivery_cost"),
            public_benefits_cost: opt_f64(row, "public_benefits_cost"),
            avg_temp_f: opt_f64(row, "avg_temp_f"),
            heating_degree_days: opt_f64(row, "heating_degree_days"),
            cooling_degree_days: opt_f64(row, "cooling_degree_days"),
            source_filename: opt_string(row, "source_filename"),
        })
        .collect::<Vec<_>>();

    let aggregate_rows = sqlx::query(
        r#"
        SELECT b.bill_month,
               SUM(b.total_cost) AS total_cost,
               SUM(b.electric_cost) AS electric_cost,
               SUM(b.gas_cost) AS gas_cost,
               SUM(b.electric_kwh) AS electric_kwh,
               SUM(b.gas_therms) AS gas_therms,
               w.avg_temp_f,
               w.heating_degree_days,
               w.cooling_degree_days
        FROM utility_bills b
        LEFT JOIN weather_monthly w ON w.month = b.bill_month
        WHERE b.bill_month BETWEEN ? AND ?
        GROUP BY b.bill_month
        ORDER BY b.bill_month
        "#,
    )
    .bind(&from)
    .bind(&to)
    .fetch_all(&state.pool)
    .await?;

    let aggregates = aggregate_rows
        .iter()
        .map(|row| MonthlyAggregate {
            bill_month: row.get("bill_month"),
            total_cost: row.get("total_cost"),
            electric_cost: opt_f64(row, "electric_cost"),
            gas_cost: opt_f64(row, "gas_cost"),
            electric_kwh: opt_f64(row, "electric_kwh"),
            gas_therms: opt_f64(row, "gas_therms"),
            avg_temp_f: opt_f64(row, "avg_temp_f"),
            heating_degree_days: opt_f64(row, "heating_degree_days"),
            cooling_degree_days: opt_f64(row, "cooling_degree_days"),
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({ "bills": bills, "monthly": aggregates })))
}

async fn import_runs(State(state): State<AppState>) -> ApiResult<Json<Vec<ImportRun>>> {
    let rows = sqlx::query(
        r#"
        SELECT id, started_at, finished_at, source, status, files_seen,
               files_imported, files_skipped, errors, message
        FROM import_runs
        ORDER BY id DESC
        LIMIT 20
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(rows.iter().map(import_run_from_row).collect()))
}

async fn import_invoices(State(state): State<AppState>) -> ApiResult<Json<crate::models::ImportResponse>> {
    let run = import::import_invoice_dir(&state.pool, &state.config).await?;
    Ok(Json(crate::models::ImportResponse { run }))
}

async fn import_csv(
    State(state): State<AppState>,
    Json(request): Json<CsvImportRequest>,
) -> ApiResult<Json<crate::models::ImportResponse>> {
    let path = request
        .path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.config.invoice_dir.join("Home Energy Usage - Monthly Usage.csv"));
    let run = import::import_csv_file(&state.pool, &path).await?;
    Ok(Json(crate::models::ImportResponse { run }))
}

const UPLOAD_LIMIT_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, serde::Deserialize)]
struct UploadQuery {
    filename: String,
}

async fn upload_invoice(
    State(state): State<AppState>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Response> {
    let bad_request = |error: String| {
        (StatusCode::BAD_REQUEST, Json(json!({ "status": "error", "error": error }))).into_response()
    };
    if let Err(error) = import::sanitize_upload_filename(&query.filename) {
        return Ok(bad_request(error.to_string()));
    }
    if body.is_empty() {
        return Ok(bad_request("uploaded file is empty".to_string()));
    }

    let run = import::import_uploaded_file(&state.pool, &state.config, &query.filename, &body).await?;
    Ok(Json(crate::models::ImportResponse { run }).into_response())
}

async fn enrich_weather(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let count = weather::enrich_missing_weather(&state.pool, &state.config).await?;
    Ok(Json(json!({ "status": "ok", "months_enriched": count })))
}

async fn latest_import_run(pool: &SqlitePool) -> anyhow::Result<Option<ImportRun>> {
    let row = sqlx::query(
        r#"
        SELECT id, started_at, finished_at, source, status, files_seen,
               files_imported, files_skipped, errors, message
        FROM import_runs
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.as_ref().map(import_run_from_row))
}

fn import_run_from_row(row: &sqlx::sqlite::SqliteRow) -> ImportRun {
    ImportRun {
        id: row.get("id"),
        started_at: row.get("started_at"),
        finished_at: opt_string(row, "finished_at"),
        source: row.get("source"),
        status: row.get("status"),
        files_seen: row.get("files_seen"),
        files_imported: row.get("files_imported"),
        files_skipped: row.get("files_skipped"),
        errors: row.get("errors"),
        message: opt_string(row, "message"),
    }
}

fn opt_string(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(column).ok().flatten()
}

fn opt_i64(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<i64> {
    row.try_get::<Option<i64>, _>(column).ok().flatten()
}

fn opt_f64(row: &sqlx::sqlite::SqliteRow, column: &str) -> Option<f64> {
    row.try_get::<Option<f64>, _>(column).ok().flatten()
}
