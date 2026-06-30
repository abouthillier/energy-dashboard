use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtilityBill {
    pub id: Option<i64>,
    pub bill_month: String,
    pub provider: String,
    pub account_label: Option<String>,
    pub billing_period_start: Option<NaiveDate>,
    pub billing_period_end: Option<NaiveDate>,
    pub bill_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub service_days: Option<i64>,
    pub electric_kwh: Option<f64>,
    pub gas_therms: Option<f64>,
    pub total_cost: f64,
    pub electric_cost: Option<f64>,
    pub gas_cost: Option<f64>,
    pub supply_cost: Option<f64>,
    pub delivery_cost: Option<f64>,
    pub public_benefits_cost: Option<f64>,
    pub credits_cost: Option<f64>,
    pub source_file_id: Option<i64>,
    pub parser_confidence: f64,
}

#[derive(Debug, Serialize)]
pub struct MonthlyBill {
    pub id: i64,
    pub bill_month: String,
    pub provider: String,
    pub billing_period_start: Option<String>,
    pub billing_period_end: Option<String>,
    pub bill_date: Option<String>,
    pub due_date: Option<String>,
    pub service_days: Option<i64>,
    pub electric_kwh: Option<f64>,
    pub gas_therms: Option<f64>,
    pub total_cost: f64,
    pub electric_cost: Option<f64>,
    pub gas_cost: Option<f64>,
    pub supply_cost: Option<f64>,
    pub delivery_cost: Option<f64>,
    pub public_benefits_cost: Option<f64>,
    pub avg_temp_f: Option<f64>,
    pub heating_degree_days: Option<f64>,
    pub cooling_degree_days: Option<f64>,
    pub source_filename: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MonthlyAggregate {
    pub bill_month: String,
    pub total_cost: f64,
    pub electric_cost: Option<f64>,
    pub gas_cost: Option<f64>,
    pub electric_kwh: Option<f64>,
    pub gas_therms: Option<f64>,
    pub avg_temp_f: Option<f64>,
    pub heating_degree_days: Option<f64>,
    pub cooling_degree_days: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SummaryResponse {
    pub latest_month: Option<String>,
    pub latest_total_cost: Option<f64>,
    pub latest_electric_kwh: Option<f64>,
    pub latest_gas_therms: Option<f64>,
    pub latest_avg_temp_f: Option<f64>,
    pub year_to_date_cost: f64,
    pub imported_bill_count: i64,
    pub failed_source_count: i64,
    pub last_import: Option<ImportRun>,
}

#[derive(Debug, Serialize)]
pub struct ImportRun {
    pub id: i64,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub source: String,
    pub status: String,
    pub files_seen: i64,
    pub files_imported: i64,
    pub files_skipped: i64,
    pub errors: i64,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportResponse {
    pub run: ImportRun,
}

#[derive(Debug, Deserialize)]
pub struct CsvImportRequest {
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MonthlyQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}
