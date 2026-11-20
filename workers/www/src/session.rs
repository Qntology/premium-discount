//! 원본 `session.ts` — 앱 로그인 엔드포인트.
//!
//!   POST /session/start : 앱 세션을 발급한다 {hash, token, address, mailto}. 사용자는 mailto 링크(QR)의 주소로 메일을 보낸다.
//!   GET  /session       : 앱이 x-oauth-hash / x-oauth-token 헤더로 현재 상태를 묻는다 {state, address}
//!                         anonymous | pending(메일 대기) | signed_in(address) | unavailable(503)
//!
//! 발급은 저장하지 않는다 (auth.rs `issue_session`). 응답에 token 이 들어 있으므로 캐시하지 않는다.
//! 처리하지 않는 경로는 `Ok(None)` (원본의 `return null`) — 다음 처리기로 넘어간다.

/// `PATHS`
pub const PATHS: [&str; 2] = ["/session", "/session/start"];

#[cfg(target_arch = "wasm32")]
pub use wasm::handle;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use premium_discount_worker_common::js::Env;
    use premium_discount_worker_common::web::{Req, Resp};
    use premium_discount_worker_common::{console_log, obj, R, V};

    use crate::auth::{address_from_email, check_session, issue_session, read_session_credentials, SessionCheck};
    use crate::ratelimit::{is_rate_limited, RETRY_AFTER_SECONDS};
    use crate::util::{client_ip, cors_headers, empty_response, header_or_empty, hv, json_response, pathname};

    /// `json(body, status, origin, extra)` — 머리글: Content-Type · Cache-Control · CORS · extra 차례
    fn json(body: V, status: u16, origin: &str, extra: &[(&str, &str)]) -> Resp {
        let mut headers = hv(&[("Content-Type", "application/json"), ("Cache-Control", "no-store")]);
        headers.extend(cors_headers(origin));
        headers.extend(hv(extra));
        json_response(&body, status, headers)
    }

    fn state(state: &str, address: V) -> V {
        obj! { "state" => state, "address" => address }
    }

    /// `handleSession(request, env)`
    pub async fn handle(req: &Req, env: &Env) -> R<Option<Resp>> {
        let path = pathname(req);
        if !super::PATHS.contains(&path.as_str()) {
            return Ok(None);
        }
        let origin = header_or_empty(req, "Origin");
        if req.method == "OPTIONS" {
            return Ok(Some(empty_response(204, cors_headers(&origin))));
        }

        let start = path == "/session/start";
        if req.method != if start { "POST" } else { "GET" } {
            return Ok(Some(json(obj! { "error" => "method not allowed" }, 405, &origin, &[])));
        }

        // 발급은 저장 제한기, 상태 확인(S3 읽기 한 번)은 읽기 제한기를 쓴다
        let limiter = env.get(if start { "EVAL_WRITE_LIMITER" } else { "EVAL_READ_LIMITER" });
        if is_rate_limited(req, &limiter).await {
            let retry = RETRY_AFTER_SECONDS.to_string();
            return Ok(Some(json(obj! { "error" => "rate limit exceeded" }, 429, &origin, &[("Retry-After", retry.as_str())])));
        }

        let r: R<Resp> = async {
            if start {
                if !env.get("SESSION_SECRET").truthy() {
                    return Ok(json(obj! { "error" => "login is not configured" }, 503, &origin, &[]));
                }
                let s = issue_session(env)?;
                let body = obj! { "hash" => s.hash, "token" => s.token, "address" => s.address, "mailto" => s.mailto };
                return Ok(json(body, 201, &origin, &[]));
            }
            let Some(creds) = read_session_credentials(req) else {
                return Ok(json(state("anonymous", V::Null), 200, &origin, &[]));
            };
            let ip = client_ip(req);
            Ok(match check_session(&creds, env, &ip).await {
                SessionCheck::Verified(email) => json(state("signed_in", V::from(address_from_email(&email))), 200, &origin, &[]),
                SessionCheck::Pending => json(state("pending", V::Null), 200, &origin, &[]),
                SessionCheck::Invalid => json(state("anonymous", V::Null), 200, &origin, &[]),
                SessionCheck::Unavailable => json(state("unavailable", V::Null), 503, &origin, &[]),
            })
        }
        .await;
        Ok(Some(match r {
            Ok(resp) => resp,
            Err(err) => {
                console_log!("session error", err);
                json(obj! { "error" => "internal error" }, 500, &origin, &[])
            }
        }))
    }
}
