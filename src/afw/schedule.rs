//! Schedule computation for cron expressions, intervals, and one-shot times.

use chrono::{DateTime, Utc};
use croner::Cron;

use crate::aria::types::CronSchedule;

/// Compute the next run time for a schedule.
///
/// Returns `None` if the schedule has no future runs (e.g., a one-shot that's past).
pub fn compute_next_run(schedule: &CronSchedule, now_ms: i64) -> Option<i64> {
    match schedule {
        CronSchedule::At { at_ms } => {
            if *at_ms > now_ms {
                Some(*at_ms)
            } else {
                None // One-shot already passed
            }
        }
        CronSchedule::Every {
            every_ms,
            anchor_ms,
        } => {
            let anchor = anchor_ms.unwrap_or(0);
            if *every_ms <= 0 {
                return None;
            }
            // Align to the interval grid from the anchor point.
            let elapsed = now_ms - anchor;
            let periods = elapsed / every_ms;
            let next = anchor + (periods + 1) * every_ms;
            Some(next)
        }
        CronSchedule::Cron { expr, tz } => {
            let cron = Cron::new(expr).parse().ok()?;

            let now_dt: DateTime<Utc> = DateTime::from_timestamp_millis(now_ms)?;

            // If timezone is specified, convert; otherwise use UTC.
            let next = if let Some(tz_str) = tz {
                let tz: chrono_tz::Tz = tz_str.parse().ok()?;
                let now_local = now_dt.with_timezone(&tz);
                let next_local = cron.find_next_occurrence(&now_local, false).ok()?;
                next_local.with_timezone(&Utc)
            } else {
                cron.find_next_occurrence(&now_dt, false).ok()?
            };

            Some(next.timestamp_millis())
        }
    }
}

/// Parse a simple cron-style schedule string into a refresh interval in seconds.
///
/// Supports basic patterns:
/// - `*/N * * * *` → every N minutes
/// - `M * * * *`   → every hour (at minute M)
/// - `M H * * *`   → every day (at H:M)
///
/// Falls back to 3600 (1 hour) for unparseable expressions.
pub fn schedule_to_interval_secs(schedule: &str) -> u64 {
    let parts: Vec<&str> = schedule.split_whitespace().collect();
    if parts.len() < 5 {
        return 3600;
    }

    // */N pattern → every N minutes
    if let Some(stripped) = parts[0].strip_prefix("*/") {
        if let Ok(n) = stripped.parse::<u64>() {
            return n * 60;
        }
    }

    // Single minute + wildcard hour → every hour
    if parts[0].parse::<u64>().is_ok() && parts[1] == "*" {
        return 3600;
    }

    // Specific minute + specific hour → daily
    if parts[0].parse::<u64>().is_ok() && parts[1].parse::<u64>().is_ok() && parts[2] == "*" {
        return 86400;
    }

    3600 // default: hourly
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_shot_future() {
        let now = 1_000_000;
        let schedule = CronSchedule::At { at_ms: 2_000_000 };
        assert_eq!(compute_next_run(&schedule, now), Some(2_000_000));
    }

    #[test]
    fn one_shot_past() {
        let now = 3_000_000;
        let schedule = CronSchedule::At { at_ms: 2_000_000 };
        assert_eq!(compute_next_run(&schedule, now), None);
    }

    #[test]
    fn interval_basic() {
        let schedule = CronSchedule::Every {
            every_ms: 60_000,
            anchor_ms: None,
        };
        let now = 150_000;
        let next = compute_next_run(&schedule, now).unwrap();
        assert!(next > now);
        assert_eq!(next, 180_000); // 3 * 60_000
    }

    #[test]
    fn interval_with_anchor() {
        let schedule = CronSchedule::Every {
            every_ms: 100,
            anchor_ms: Some(50),
        };
        let now = 275;
        let next = compute_next_run(&schedule, now).unwrap();
        assert_eq!(next, 350); // anchor(50) + 3*100 = 350
    }

    #[test]
    fn interval_zero_rejected() {
        let schedule = CronSchedule::Every {
            every_ms: 0,
            anchor_ms: None,
        };
        assert_eq!(compute_next_run(&schedule, 100), None);
    }

    #[test]
    fn cron_expression() {
        let schedule = CronSchedule::Cron {
            expr: "0 * * * *".to_string(), // every hour at :00
            tz: None,
        };
        let now = Utc::now().timestamp_millis();
        let next = compute_next_run(&schedule, now);
        assert!(next.is_some());
        assert!(next.unwrap() > now);
    }

    #[test]
    fn schedule_to_interval_every_n_minutes() {
        assert_eq!(schedule_to_interval_secs("*/5 * * * *"), 300);
        assert_eq!(schedule_to_interval_secs("*/15 * * * *"), 900);
    }

    #[test]
    fn schedule_to_interval_hourly() {
        assert_eq!(schedule_to_interval_secs("0 * * * *"), 3600);
        assert_eq!(schedule_to_interval_secs("30 * * * *"), 3600);
    }

    #[test]
    fn schedule_to_interval_daily() {
        assert_eq!(schedule_to_interval_secs("0 9 * * *"), 86400);
    }
}
