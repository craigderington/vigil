//! Monthly report scheduler: on a configurable day-of-month + UTC time, backfill
//! every month between the marker and the just-ended month (idempotent), emailing
//! each; the marker advances only on a delivered/nothing-to-send outcome.

use std::time::Duration;

use crate::app::AppState;
use crate::digest::SendOutcome;
use crate::report::{self, month_of, next_month, prior_month};
use crate::settings_store;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Days in the UTC month containing `epoch`.
fn days_in_month(epoch: i64) -> u32 {
    let d = chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0).unwrap_or_default().date_naive();
    let (y, m) = (d.format("%Y").to_string().parse::<i32>().unwrap_or(1970), d.format("%m").to_string().parse::<u32>().unwrap_or(1));
    let first_next = if m == 12 { chrono::NaiveDate::from_ymd_opt(y + 1, 1, 1) } else { chrono::NaiveDate::from_ymd_opt(y, m + 1, 1) };
    first_next.and_then(|nx| nx.pred_opt()).map(|last| chrono::Datelike::day0(&last) + 1).unwrap_or(28)
}

pub fn should_run_today(now_ts: i64, day_of_month: i64, time_offset: i64) -> bool {
    // chrono::Datelike methods called fully-qualified (no `use chrono` per Global Constraints).
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(now_ts, 0).unwrap_or_default();
    let today = dt.date_naive();
    let eff_day = day_of_month.clamp(1, days_in_month(now_ts) as i64) as u32;
    let start_of_today = today.and_hms_opt(0, 0, 0).map(|d| d.and_utc().timestamp()).unwrap_or(0);
    chrono::Datelike::day(&today) >= eff_day && now_ts >= start_of_today + time_offset
}

fn parse_hm(s: &str) -> i64 {
    let mut it = s.split(':');
    let h = it.next().and_then(|x| x.parse::<i64>().ok()).filter(|h| (0..24).contains(h));
    let m = it.next().and_then(|x| x.parse::<i64>().ok()).filter(|m| (0..60).contains(m));
    match (h, m) { (Some(h), Some(m)) => h * 3600 + m * 60, _ => 8 * 3600 }
}

pub async fn seed_marker_if_absent(state: &AppState) -> anyhow::Result<()> {
    let existing: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'report.last_generated_period'").fetch_optional(&state.db).await?;
    if existing.is_none() {
        settings_store::set(&state.db, "report.last_generated_period", &prior_month(&month_of(now()))).await?;
    }
    Ok(())
}

pub async fn tick_once(state: &AppState) -> anyhow::Result<()> {
    if !settings_store::report_auto_generate(&state.db).await { return Ok(()); }
    let now_ts = now();
    let target = prior_month(&month_of(now_ts));
    let day = settings_store::report_day_of_month(&state.db).await;
    let off = parse_hm(&settings_store::report_time(&state.db).await);
    if !should_run_today(now_ts, day, off) { return Ok(()); }
    let mut cursor = settings_store::get(&state.db, "report.last_generated_period", "").await;
    if cursor.is_empty() { cursor = prior_month(&target); } // safety; run() seeds first
    while next_month(&cursor).as_str() <= target.as_str() {
        let p = next_month(&cursor);
        let r = report::generate(state, &p).await?;
        let recips: Vec<i64> = serde_json::from_str(&settings_store::get(&state.db, "report_recipients", "[]").await).unwrap_or_default();
        let outcome = if recips.is_empty() { SendOutcome::NothingToSend } else { report::send_report_email(state, &r).await };
        match outcome {
            SendOutcome::Delivered | SendOutcome::NothingToSend => {
                settings_store::set(&state.db, "report.last_generated_period", &p).await?;
                cursor = p;
            }
            SendOutcome::AllFailed => break, // hold marker; retry next tick
        }
    }
    Ok(())
}

pub async fn run(state: AppState) {
    if let Err(e) = seed_marker_if_absent(&state).await { tracing::error!(error = %e, "report marker seed failed"); }
    loop {
        let tick = settings_store::report_tick_seconds(&state.db).await;
        if let Err(e) = tick_once(&state).await { tracing::error!(error = %e, "report tick failed"); }
        tokio::time::sleep(Duration::from_secs(tick.max(1) as u64)).await;
    }
}
