use chrono::{Datelike, NaiveDate};
use regex::Regex;

use crate::models::UtilityBill;

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("unknown invoice provider")]
    UnknownProvider,
    #[error("missing required field: {0}")]
    MissingField(&'static str),
}

pub fn parse_invoice(text: &str) -> Result<UtilityBill, ParseError> {
    if text.contains("MANSFIELD MUNICIPAL ELECTRIC") || text.contains("Mansfield Electric") {
        parse_mansfield_electric(text)
    } else if text.contains("Total Cost of Gas") || text.contains("Eversource") {
        parse_eversource_gas(text)
    } else {
        Err(ParseError::UnknownProvider)
    }
}

fn parse_mansfield_electric(text: &str) -> Result<UtilityBill, ParseError> {
    let account_label = capture(
        text,
        r"(?m)^\s*Account Number\s+Customer\s+.*\n\s*(\d+)\s+",
    );
    let bill_date = capture(text, r"(?m)^\s*\d+\s+ALEX.*?\s+(\d{2}/\d{2}/\d{4})\s+\d{2}/\d{2}/\d{4}")
        .and_then(|value| parse_date(&value));
    let due_date = capture(text, r"(?m)^\s*\d+\s+ALEX.*?\s+\d{2}/\d{2}/\d{4}\s+(\d{2}/\d{2}/\d{4})")
        .and_then(|value| parse_date(&value));

    let meter_re = Regex::new(
        r"(?m)^\s*\d+\s+(\d{2}/\d{2}/\d{4})\s+(\d{2}/\d{2}/\d{4})\s+(\d+)\s+[\d,]+\s+[\d,]+\s+[\d,]+\s+\d+\s+([\d,]+)\s+KWH",
    )
    .unwrap();
    let meter = meter_re
        .captures(text)
        .ok_or(ParseError::MissingField("electric meter usage line"))?;
    let billing_period_end = parse_date(&meter[1]);
    let billing_period_start = parse_date(&meter[2]);
    let service_days = meter[3].parse::<i64>().ok();
    let electric_kwh = parse_number(&meter[4]);

    let total_cost = capture_money(text, r"NEW CHARGES TOTAL\s+\$?(-?[\d,]+\.\d{2})")
        .or_else(|| capture_money(text, r"TOTAL AMOUNT DUE\s+\$?(-?[\d,]+\.\d{2})"))
        .ok_or(ParseError::MissingField("electric total cost"))?;
    let customer_charge = capture_money(text, r"Customer Charge\s+\$?(-?[\d,]+\.\d{2})").unwrap_or(0.0);
    let distribution = capture_money(text, r"(?m)Distribution Charge[^\n]*\s+\$(-?[\d,]+\.\d{2})").unwrap_or(0.0);
    let purchased_power = capture_money(text, r"(?m)Purchased Power Charge[^\n]*\s+\$(-?[\d,]+\.\d{2})").unwrap_or(0.0);
    let purchased_adjustment = capture_money(text, r"(?m)Purchased Power Adjustment[^\n]*\s+\$(-?[\d,]+\.\d{2})").unwrap_or(0.0);
    let revenue_credit = capture_money(text, r"Residential Revenue Credit\s+[\d.]+\s+\d+\s+(-?\$?[\d,]+\.\d{2})").unwrap_or(0.0);
    let interest_credit = capture_money(text, r"Interest on Deposit\s+(-?\$?[\d,]+\.\d{2})").unwrap_or(0.0);
    let discount = capture_money(text, r"Early Payment Discount\s+(-?\$?[\d,]+\.\d{2})").unwrap_or(0.0);
    let bill_month = bill_month(billing_period_end.or(bill_date));

    Ok(UtilityBill {
        id: None,
        bill_month,
        provider: "Mansfield Municipal Electric".to_string(),
        account_label,
        billing_period_start,
        billing_period_end,
        bill_date,
        due_date,
        service_days,
        electric_kwh,
        gas_therms: None,
        total_cost,
        electric_cost: Some(total_cost),
        gas_cost: None,
        supply_cost: Some(purchased_power + purchased_adjustment),
        delivery_cost: Some(customer_charge + distribution),
        public_benefits_cost: None,
        credits_cost: Some(revenue_credit + interest_credit + discount),
        source_file_id: None,
        parser_confidence: 0.95,
    })
}

fn parse_eversource_gas(text: &str) -> Result<UtilityBill, ParseError> {
    let account_label = capture(text, r"(?m)Account Number:\s*\n?.*?\n?.*?(\d{4}\s+\d{3}\s+\d{4})")
        .or_else(|| capture(text, r"(?m)(\d{4}\s+\d{3}\s+\d{4})\s+\$"));
    let bill_date = capture(text, r"Statement Date:\s*(\d{2}/\d{2}/\d{2})").and_then(|value| parse_date(&value));
    let service_re = Regex::new(r"Service from\s+(\d{2}/\d{2}/\d{2})\s+-\s+(\d{2}/\d{2}/\d{2})\s+(\d+)\s+Days").unwrap();
    let service = service_re
        .captures(text)
        .ok_or(ParseError::MissingField("gas service period"))?;
    let billing_period_start = parse_date(&service[1]);
    let billing_period_end = parse_date(&service[2]);
    let service_days = service[3].parse::<i64>().ok();

    let gas_therms = capture(text, r"=\s+([\d,]+)\s+Therms Billed Usage")
        .and_then(|value| parse_number(&value))
        .or_else(|| capture(text, r"This month you used\s+.*?\n.*?([\d,]+)\s+Therms").and_then(|value| parse_number(&value)));
    let total_cost = capture_money(text, r"Total Cost of Gas\s+\$?(-?[\d,]+\.\d{2})")
        .ok_or(ParseError::MissingField("gas total cost"))?;
    let supply_cost = capture_money(text, r"(?m)Subtotal Supply\s+\$?(-?[\d,]+\.\d{2})\s*$");
    let delivery_cost = capture_money(text, r"(?m)Subtotal Maintenance & Infrastructure Investment\s+\$?(-?[\d,]+\.\d{2})\s*$");
    let public_benefits_cost = capture_money(text, r"(?m)Subtotal Public Benefits\s+\$?(-?[\d,]+\.\d{2})\s*$");
    let bill_month = bill_month(billing_period_end.or(bill_date));

    Ok(UtilityBill {
        id: None,
        bill_month,
        provider: "Eversource Gas".to_string(),
        account_label,
        billing_period_start,
        billing_period_end,
        bill_date,
        due_date: None,
        service_days,
        electric_kwh: None,
        gas_therms,
        total_cost,
        electric_cost: None,
        gas_cost: Some(total_cost),
        supply_cost,
        delivery_cost,
        public_benefits_cost,
        credits_cost: None,
        source_file_id: None,
        parser_confidence: 0.95,
    })
}

fn capture(text: &str, pattern: &str) -> Option<String> {
    Regex::new(pattern)
        .ok()?
        .captures(text)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim().to_string())
}

fn capture_money(text: &str, pattern: &str) -> Option<f64> {
    capture(text, pattern).and_then(|value| parse_money(&value))
}

fn parse_money(value: &str) -> Option<f64> {
    let negative = value.trim().starts_with("-") || value.trim().starts_with("-$");
    let cleaned = value.replace(['$', ',', '-'], "");
    let amount = cleaned.trim().parse::<f64>().ok()?;
    Some(if negative { -amount } else { amount })
}

fn parse_number(value: &str) -> Option<f64> {
    value.replace(',', "").trim().parse::<f64>().ok()
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    let year_token = value.split('/').nth(2)?;
    if year_token.len() == 4 {
        NaiveDate::parse_from_str(value, "%m/%d/%Y").ok()
    } else {
        parse_short_year_date(value)
    }
}

fn parse_short_year_date(value: &str) -> Option<NaiveDate> {
    let mut parts = value.split('/');
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    let year = parts.next()?.parse::<i32>().ok()?;
    let full_year = if year >= 70 { 1900 + year } else { 2000 + year };
    NaiveDate::from_ymd_opt(full_year, month, day)
}

fn bill_month(date: Option<NaiveDate>) -> String {
    date.map(|date| format!("{:04}-{:02}", date.year(), date.month()))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mansfield_electric_sample() {
        let text = r#"
 Account Number                                  Customer                                                   Service Location                           Book          Bill Date               Due Date
    14001301                                 ALEX BOUTHILLIER                                                 50 OTIS ST                               0200         06/10/2026              07/05/2026
         80626585                            06/03/2026              05/05/2026                   29        75,149               74,485                664            1         664        KWH         RES
                                                                                                     300    Customer Charge                                                                           $10.00
                                                                                                     301    Distribution Charge (First 1000)          0.03000                     664                 $19.92
                                                                                                     302    Purchased Power Charge                    0.10900                     664                 $72.38
                                                                                                     303    Purchased Power Adjustment                0.02000                     664                 $13.28
                                                                                                     375    Residential Revenue Credit                0.00400                     664                 -$2.66
                                                                                                     004    Interest on Deposit                                                                       -$0.93
                                                                                                     101    Early Payment Discount                                                                    -$3.98
MANSFIELD MUNICIPAL ELECTRIC
NEW CHARGES TOTAL                                                                       $108.01
"#;
        let bill = parse_invoice(text).unwrap();
        assert_eq!(bill.provider, "Mansfield Municipal Electric");
        assert_eq!(bill.electric_kwh, Some(664.0));
        assert_eq!(bill.total_cost, 108.01);
        assert_eq!(bill.bill_month, "2026-06");
    }

    #[test]
    fn parses_eversource_gas_sample() {
        let text = r#"
Eversource
Statement Date: 06/11/26
Service from 05/13/26 - 06/11/26 30 Days
20 x Therm factor of 1.0270 = 21 Therms Billed Usage for 30 Days
Gas Account Summary
Total Cost of Gas $74.65
Supplier Services $8.50 21 Therms X .40480
Subtotal Supply $8.50
Subtotal Maintenance & Infrastructure Investment $56.92
Subtotal Public Benefits $9.23
Total Cost of Gas $74.65
"#;
        let bill = parse_invoice(text).unwrap();
        assert_eq!(bill.provider, "Eversource Gas");
        assert_eq!(bill.gas_therms, Some(21.0));
        assert_eq!(bill.total_cost, 74.65);
        assert_eq!(bill.bill_month, "2026-06");
    }
}
