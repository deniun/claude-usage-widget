use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdout, Command};
use tokio::time::timeout;

use crate::errors::{AppError, AppResult};
use crate::types::{Provider, Status, UsageResponse, UsageWindow};

#[derive(Deserialize, Debug)]
pub(crate) struct PrimaryOrSecondary {
    #[serde(rename = "usedPercent")]
    pub used_percent: Option<f64>,
    #[serde(rename = "windowDurationMins")]
    pub window_duration_mins: Option<u64>,
    #[serde(rename = "resetsAt")]
    pub resets_at: Option<i64>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Bucket {
    #[serde(rename = "limitId")]
    pub limit_id: Option<String>,
    #[serde(rename = "limitName")]
    pub limit_name: Option<String>,
    pub primary: Option<PrimaryOrSecondary>,
    pub secondary: Option<PrimaryOrSecondary>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct RateLimitsResult {
    #[serde(rename = "rateLimitsByLimitId", default)]
    pub rate_limits_by_limit_id: std::collections::HashMap<String, Bucket>,
}

fn window_name(dur_mins: u64) -> String {
    let hours = dur_mins / 60;
    if hours >= 24 {
        format!("{}일", hours / 24)
    } else {
        format!("{}시간", hours)
    }
}

fn iso_from_epoch(sec: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(sec, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

fn compute_time_progress(resets_epoch: i64, duration_sec: u64) -> f64 {
    let now = chrono::Utc::now().timestamp();
    let start = resets_epoch - duration_sec as i64;
    if now <= start { return 0.0; }
    if now >= resets_epoch { return 100.0; }
    ((now - start) as f64 / duration_sec as f64 * 100.0).round()
}

fn bucket_label(bucket: &Bucket) -> String {
    match bucket.limit_id.as_deref() {
        Some("codex") => "Codex 전체".into(),
        Some("base_model_inference") => "GPT 예비".into(),
        _ => bucket
            .limit_name
            .as_deref()
            .unwrap_or("기타")
            .trim_start_matches("GPT-5.3-Codex-")
            .to_string(),
    }
}

fn push_window(out: &mut Vec<UsageWindow>, key: &str, label: &str, pw: &PrimaryOrSecondary) {
    let (util, dur_mins, reset_sec) = match (pw.used_percent, pw.window_duration_mins, pw.resets_at) {
        (Some(u), Some(d), Some(r)) => (u, d, r),
        _ => return,
    };
    let dur_sec = dur_mins * 60;
    out.push(UsageWindow {
        key: key.to_string(),
        name: format!("{} ({})", window_name(dur_mins), label),
        utilization: util,
        resets_at: iso_from_epoch(reset_sec),
        time_progress: compute_time_progress(reset_sec, dur_sec),
    });
}

/// 24시간 미만이면 "짧은 창"(5시간 같은 세션 한도)으로 본다.
const SHORT_WINDOW_MAX_MINS: u64 = 24 * 60;

fn short_window(slot: &Option<PrimaryOrSecondary>) -> Option<&PrimaryOrSecondary> {
    let w = slot.as_ref()?;
    match w.window_duration_mins {
        Some(d) if d < SHORT_WINDOW_MAX_MINS => Some(w),
        _ => None,
    }
}

pub(crate) fn map_to_response(result: &RateLimitsResult) -> UsageResponse {
    let mut windows = Vec::new();
    let codex_bucket = result.rate_limits_by_limit_id.get("codex");
    if let Some(b) = codex_bucket {
        let base = b.limit_id.clone().unwrap_or_else(|| "unknown".into());
        let label = bucket_label(b);
        if let Some(p) = &b.primary { push_window(&mut windows, &format!("{}_primary", base), &label, p); }
        if let Some(s) = &b.secondary { push_window(&mut windows, &format!("{}_secondary", base), &label, s); }
    }

    // 플랜에 따라 `codex` 버킷이 주간 창만 준다(예: planType=prolite → primary가
    // 10080분 하나뿐이고 secondary는 null). 그런 계정에서 5시간 창은 모델별
    // 버킷에만 존재하므로 위젯에 세션 한도가 통째로 안 보였다.
    // `codex` 버킷에 짧은 창이 하나도 없을 때만 모델별 버킷에서 가장 짧은 창
    // 하나를 보충한다. 모델별 주간 창은 `codex` 주간과 중복이라 계속 감춘다.
    let has_short_window = codex_bucket.is_some_and(|b| {
        short_window(&b.primary).is_some() || short_window(&b.secondary).is_some()
    });
    if !has_short_window {
        let mut candidates: Vec<(u64, &String, &Bucket, &PrimaryOrSecondary)> = Vec::new();
        for (id, b) in &result.rate_limits_by_limit_id {
            if id == "codex" || id == "base_model_inference" {
                continue;
            }
            for slot in [&b.primary, &b.secondary] {
                if let Some(w) = short_window(slot) {
                    candidates.push((w.window_duration_mins.unwrap_or(u64::MAX), id, b, w));
                }
            }
        }
        // HashMap 순회 순서는 비결정적이다. 기간 → 버킷 id 순으로 고정한다.
        candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        if let Some((_, id, bucket, w)) = candidates.first() {
            let label = bucket_label(bucket);
            let mut short = Vec::new();
            push_window(&mut short, &format!("codex_short_{}", id), &label, w);
            // 짧은 창이 위에 오게 한다(5시간 → 7일).
            windows.splice(0..0, short);
        }
    }

    UsageResponse {
        provider: Provider::Codex,
        status: Status::Ok,
        windows,
        extra_usage: None,
        error: None,
    }
}

/// Older persisted snapshots may still contain model-specific buckets. Keep
/// startup display consistent with fresh responses and upgrade the old label.
pub(crate) fn normalize_cached_response(response: &mut UsageResponse) {
    response.windows.retain(|w| {
        matches!(w.key.as_str(), "codex_primary" | "codex_secondary")
            || w.key.starts_with("codex_short_")
    });
    for window in &mut response.windows {
        // 보충된 짧은 창은 모델 이름(예: "5시간 (Spark)")을 그대로 둔다.
        if matches!(window.key.as_str(), "codex_primary" | "codex_secondary") {
            let duration = window.name.split(" (").next().unwrap_or(&window.name);
            window.name = format!("{} (Codex 전체)", duration);
        }
    }
}

async fn read_response_by_id(
    reader: &mut tokio::io::Lines<BufReader<ChildStdout>>,
    target_id: u64,
    deadline: Duration,
) -> AppResult<serde_json::Value> {
    let fut = async {
        loop {
            let line = reader
                .next_line()
                .await
                .map_err(|e| AppError::Other(format!("read stdout: {}", e)))?;
            let line = match line {
                Some(l) => l,
                None => return Err(AppError::Other("codex stdout closed".into())),
            };
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            let msg: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if msg.get("id").and_then(|v| v.as_u64()) == Some(target_id) {
                if let Some(err) = msg.get("error") {
                    return Err(AppError::Other(format!("codex rpc error: {}", err)));
                }
                if let Some(result) = msg.get("result") {
                    return Ok(result.clone());
                }
                return Err(AppError::Other("codex response missing result".into()));
            }
        }
    };
    timeout(deadline, fut)
        .await
        .map_err(|_| AppError::Other("codex rpc timed out".into()))?
}

pub async fn fetch() -> AppResult<UsageResponse> {
    let codex_bin = if cfg!(windows) { "codex.cmd" } else { "codex" };
    let mut cmd = Command::new(codex_bin);
    cmd.args(["app-server", "-c", "sandbox=\"danger-full-access\""])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = cmd.spawn().map_err(|e| {
        AppError::NotAuthenticated(format!("codex CLI not found on PATH: {}", e))
    })?;

    let mut stdin = child.stdin.take().ok_or_else(|| AppError::Other("no stdin".into()))?;
    let stdout = child.stdout.take().ok_or_else(|| AppError::Other("no stdout".into()))?;
    let mut reader = BufReader::new(stdout).lines();

    let init_req = r#"{"method":"initialize","params":{"clientInfo":{"name":"claude-usage-widget","version":"0.1.0"}},"id":1}"#;
    let init_notif = r#"{"method":"initialized","params":{}}"#;
    let rl_req = r#"{"method":"account/rateLimits/read","params":{},"id":2}"#;

    stdin.write_all(format!("{}\n", init_req).as_bytes()).await.map_err(AppError::Io)?;
    // wait for init response before sending initialized (matches bridge flow)
    let _ = read_response_by_id(&mut reader, 1, Duration::from_secs(10)).await?;
    stdin.write_all(format!("{}\n", init_notif).as_bytes()).await.map_err(AppError::Io)?;
    stdin.write_all(format!("{}\n", rl_req).as_bytes()).await.map_err(AppError::Io)?;

    let result = read_response_by_id(&mut reader, 2, Duration::from_secs(10)).await?;

    let _ = child.start_kill();
    let _ = child.wait().await;

    let parsed: RateLimitsResult = serde_json::from_value(result.clone()).map_err(|e| {
        AppError::Other(format!("codex rateLimits parse failed: {} | raw: {}", e, result))
    })?;

    if parsed.rate_limits_by_limit_id.is_empty() {
        return Ok(UsageResponse {
            provider: Provider::Codex,
            status: Status::Ok,
            windows: vec![],
            extra_usage: None,
            error: None,
        });
    }

    Ok(map_to_response(&parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_primary_and_secondary() {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "codex".to_string(),
            Bucket {
                limit_id: Some("codex".into()),
                limit_name: Some("Plan A".into()),
                primary: Some(PrimaryOrSecondary {
                    used_percent: Some(20.0),
                    window_duration_mins: Some(300),
                    resets_at: Some(4_000_000_000),
                }),
                secondary: Some(PrimaryOrSecondary {
                    used_percent: Some(55.0),
                    window_duration_mins: Some(10_080),
                    resets_at: Some(4_000_000_000),
                }),
            },
        );
        let result = RateLimitsResult { rate_limits_by_limit_id: map };
        let resp = map_to_response(&result);
        assert_eq!(resp.windows.len(), 2);
        assert_eq!(resp.windows[0].name, "5시간 (Codex 전체)");
        assert_eq!(resp.windows[1].name, "7일 (Codex 전체)");
    }

    #[test]
    fn supplements_short_window_from_model_bucket_when_codex_has_none() {
        fn weekly_bucket(id: &str, used_percent: f64) -> Bucket {
            Bucket {
                limit_id: Some(id.into()),
                limit_name: None,
                primary: Some(PrimaryOrSecondary {
                    used_percent: Some(used_percent),
                    window_duration_mins: Some(10_080),
                    resets_at: Some(4_000_000_000),
                }),
                secondary: None,
            }
        }

        let mut map = std::collections::HashMap::new();
        map.insert("codex".into(), weekly_bucket("codex", 9.0));
        map.insert(
            "base_model_inference".into(),
            weekly_bucket("base_model_inference", 0.0),
        );
        map.insert(
            "codex_bengalfox".into(),
            Bucket {
                limit_id: Some("codex_bengalfox".into()),
                limit_name: Some("GPT-5.3-Codex-Spark".into()),
                primary: Some(PrimaryOrSecondary {
                    used_percent: Some(0.0),
                    window_duration_mins: Some(300),
                    resets_at: Some(4_000_000_000),
                }),
                secondary: Some(PrimaryOrSecondary {
                    used_percent: Some(0.0),
                    window_duration_mins: Some(10_080),
                    resets_at: Some(4_000_000_000),
                }),
            },
        );

        let result = RateLimitsResult { rate_limits_by_limit_id: map };
        let resp = map_to_response(&result);

        // `codex` 버킷에 5시간 창이 없으므로 모델별 버킷에서 하나만 보충한다.
        // 모델별 주간 창과 "GPT 예비"(base_model_inference)는 계속 감춘다.
        assert_eq!(resp.windows.len(), 2);
        assert_eq!(resp.windows[0].name, "5시간 (Spark)");
        assert_eq!(resp.windows[0].key, "codex_short_codex_bengalfox");
        assert_eq!(resp.windows[1].name, "7일 (Codex 전체)");
        assert_eq!(resp.windows[1].key, "codex_primary");
    }

    /// `codex` 버킷이 이미 5시간 창을 주면 모델별 버킷은 건드리지 않는다.
    #[test]
    fn does_not_supplement_when_codex_bucket_has_short_window() {
        let mut map = std::collections::HashMap::new();
        map.insert(
            "codex".to_string(),
            Bucket {
                limit_id: Some("codex".into()),
                limit_name: None,
                primary: Some(PrimaryOrSecondary {
                    used_percent: Some(20.0),
                    window_duration_mins: Some(300),
                    resets_at: Some(4_000_000_000),
                }),
                secondary: Some(PrimaryOrSecondary {
                    used_percent: Some(55.0),
                    window_duration_mins: Some(10_080),
                    resets_at: Some(4_000_000_000),
                }),
            },
        );
        map.insert(
            "codex_bengalfox".into(),
            Bucket {
                limit_id: Some("codex_bengalfox".into()),
                limit_name: Some("GPT-5.3-Codex-Spark".into()),
                primary: Some(PrimaryOrSecondary {
                    used_percent: Some(0.0),
                    window_duration_mins: Some(300),
                    resets_at: Some(4_000_000_000),
                }),
                secondary: None,
            },
        );

        let resp = map_to_response(&RateLimitsResult { rate_limits_by_limit_id: map });
        assert_eq!(resp.windows.len(), 2);
        assert_eq!(resp.windows[0].key, "codex_primary");
        assert_eq!(resp.windows[1].key, "codex_secondary");
    }

    /// 보충된 짧은 창은 캐시 정규화에서 살아남고 모델 이름도 유지한다.
    #[test]
    fn normalize_keeps_supplemented_short_window() {
        let mut response = UsageResponse {
            provider: Provider::Codex,
            status: Status::Ok,
            windows: vec![
                UsageWindow {
                    key: "codex_short_codex_bengalfox".into(),
                    name: "5시간 (Spark)".into(),
                    utilization: 0.0,
                    resets_at: "2030-01-01T00:00:00Z".into(),
                    time_progress: 1.0,
                },
                UsageWindow {
                    key: "codex_primary".into(),
                    name: "7일".into(),
                    utilization: 50.0,
                    resets_at: "2030-01-01T00:00:00Z".into(),
                    time_progress: 43.0,
                },
            ],
            extra_usage: None,
            error: None,
        };

        normalize_cached_response(&mut response);

        assert_eq!(response.windows.len(), 2);
        assert_eq!(response.windows[0].name, "5시간 (Spark)");
        assert_eq!(response.windows[1].name, "7일 (Codex 전체)");
    }

    #[test]
    fn normalizes_old_cached_response() {
        let mut response = UsageResponse {
            provider: Provider::Codex,
            status: Status::Ok,
            windows: vec![
                UsageWindow {
                    key: "codex_primary".into(),
                    name: "7일".into(),
                    utilization: 22.0,
                    resets_at: "2030-01-01T00:00:00Z".into(),
                    time_progress: 1.0,
                },
                UsageWindow {
                    key: "codex_bengalfox_primary".into(),
                    name: "5시간 (Spark)".into(),
                    utilization: 0.0,
                    resets_at: "2030-01-01T00:00:00Z".into(),
                    time_progress: 1.0,
                },
            ],
            extra_usage: None,
            error: None,
        };

        normalize_cached_response(&mut response);

        assert_eq!(response.windows.len(), 1);
        assert_eq!(response.windows[0].name, "7일 (Codex 전체)");
    }
}
